// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! mogwai fake-broker server.
//!
//! Hosts the native JSON-over-WS gateway (`/ws`) that the broadarrow adapter
//! connects to, plus an out-of-band control plane (`/control/divergence`) for
//! arming deterministic divergences from tests. The exchange logic lives in
//! [`mogwai_engine`]; market data is synthesized from the committed fingerprint
//! by [`mogwai_data`]; this binary owns sockets, the clock and replay pacing.

#[cfg(not(unix))]
compile_error!("mogwai-server requires a Unix target");

mod config;
// `gen` is a reserved keyword in the 2024 edition (generator blocks), so the
// module is declared via the raw identifier; `crate::r#gen` is otherwise the
// plain `crate::gen` module the spec names throughout.
mod accounts;
mod admission;
mod r#gen;
mod http;
mod man;
mod source;
mod tape;
mod ws;

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Router,
    routing::{get, post},
};
use clap::{Args, Parser, Subcommand};
use nix::{
    errno::Errno,
    fcntl::{Flock, FlockArg},
    sys::signal::{Signal, kill},
    unistd::{ForkResult, Pid},
};

use crate::accounts::{AccountRegistry, AccountTemplate};
use crate::config::{
    Config, build_instrument_profiles, build_sim_clock, build_tape_permits, now_ns,
    warn_unfunded_quotes,
};
use crate::http::{
    AppState, account, arm_divergence, clock, delete_account, instruments, list_accounts, quotes,
    submit_order_http, trades,
};
use crate::ws::ws_upgrade;

/// Version banner: semver plus the short git hash (with a `-dirty` suffix for an
/// unclean tree) and the UTC build time, stamped at compile time by `build.rs`.
/// e.g. `0.1.0 (abc123def 2026-06-24 12:34:56 UTC)`. Fed to clap's `--version`.
const LONG_VERSION: &str = env!("MOGWAI_LONG_VERSION");

/// The `mogwai` command line. Explicit verbs run the gateway, stop its daemon,
/// or render the bundled reference docs. There is deliberately no default verb:
/// a bare `mogwai` prints help rather than silently binding a socket.
/// `--help`, `--version`/`-V` and the argument grammar are clap-provided; the
/// server's run knobs live in `mogwai.toml`, not in flags or the environment.
#[derive(Parser)]
#[command(
    name = "mogwai",
    version = LONG_VERSION,
    about = "Fake broker/exchange that drives broadarrow's live trading path",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The explicit verbs: serve the gateway, stop a daemon, or read bundled docs.
#[derive(Subcommand)]
enum Command {
    /// Run the gateway server. Daemonizes by default; pass -f for foreground.
    Serve(ServeArgs),
    /// Stop a daemon started by `mogwai serve`, by its PID file.
    Stop(StopArgs),
    /// Dump the offline generator as CSV (trades or bars).
    Gen(r#gen::GenArgs),
    /// Render a bundled reference doc, or list the topics when none is given.
    Man {
        /// Reference topic to display. Omit to list the available topics.
        #[arg(value_name = "TOPIC")]
        topic: Option<man::ManTopic>,
    },
}

/// `serve` arguments: where to read run config and what address to bind.
#[derive(Args)]
struct ServeArgs {
    /// Load run config from this TOML file. Defaults to `mogwai.toml` in the
    /// working directory; a missing file falls back to built-in defaults, a
    /// malformed one is a hard error.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Address to bind the gateway to, as `host:port`. The adapter's default
    /// server URL targets `8787`, so a non-default port also needs the adapter
    /// pointed at it.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8787")]
    addr: SocketAddr,
    /// Write structured logs to this file instead of the terminal. A server
    /// launched in the background has its stdout block-buffered by the launcher,
    /// so a tracing line can sit unflushed indefinitely - invisible to a tail or
    /// grep watching for readiness. A dedicated file is written per event and
    /// stays greppable in real time; the one-line readiness banner still prints
    /// to stdout. Defaults to `mogwai.log` in the working directory.
    #[arg(long, value_name = "PATH", default_value = "mogwai.log")]
    log_file: PathBuf,
    /// PID-file path for the daemon and single-instance lock. Ignored under -f.
    #[arg(long, value_name = "PATH", default_value = "mogwai.pid")]
    pid_file: PathBuf,
    /// Stay in the foreground instead of daemonizing.
    #[arg(short = 'f', long)]
    foreground: bool,
}

#[derive(Args)]
struct StopArgs {
    #[arg(long, value_name = "PATH", default_value = "mogwai.pid")]
    pid_file: PathBuf,
}

struct ResolvedServeArgs {
    config: Option<PathBuf>,
    addr: SocketAddr,
    log_file: PathBuf,
    pid_file: PathBuf,
    foreground: bool,
}

/// Owns the readiness pipe write end. A clean bind writes 0; dropping before
/// that writes 1 so the parent never waits forever on startup failure.
struct PipeReady {
    fd: Option<OwnedFd>,
}

impl PipeReady {
    fn new(fd: OwnedFd) -> Self {
        Self { fd: Some(fd) }
    }

    fn signal_ready(&mut self) -> anyhow::Result<()> {
        if let Some(fd) = self.fd.take() {
            let written = nix::unistd::write(&fd, &[0u8])?;
            if written != 1 {
                anyhow::bail!("readiness pipe wrote {written} bytes instead of 1");
            }
        }
        Ok(())
    }
}

impl Drop for PipeReady {
    fn drop(&mut self) {
        if let Some(fd) = self.fd.take() {
            ignore_error(nix::unistd::write(&fd, &[1u8]));
        }
    }
}

enum Ready {
    Stdout,
    Pipe(PipeReady),
}

/// Held for the daemon lifetime. The wrapped fd owns the advisory PID lock.
struct PidLock(Flock<File>);

enum PidLockStatus {
    Acquired(PidLock),
    Held(Option<i32>),
}

enum LockAttempt {
    Acquired(PidLock),
    Held(File),
}

enum WaitLock {
    Released(PidLock),
    StillHeld(File),
}

const READY_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_KILL_GRACE: Duration = Duration::from_secs(2);
const PID_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// How many times `acquire_pid_lock` re-opens after locking an inode that was
/// unlinked out from under it before giving up. One retry resolves the normal
/// race; the small cap only stops a pathological adversary unlinking in a loop.
const PID_LOCK_ACQUIRE_ATTEMPTS: usize = 8;

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Man { topic } => {
            man::run(topic);
            Ok(())
        }
        Command::Stop(args) => stop(args),
        Command::Serve(args) => serve(args),
        Command::Gen(args) => r#gen::run(args),
    }
}

fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let args = resolve_paths(args)?;
    if args.foreground {
        init_logging(&args.log_file)?;
        return run_runtime(args, Ready::Stdout, None);
    }

    let lock = match acquire_pid_lock(&args.pid_file)? {
        PidLockStatus::Acquired(lock) => lock,
        PidLockStatus::Held(Some(pid)) => {
            anyhow::bail!("already running, pid {pid}; run `mogwai stop` first");
        }
        PidLockStatus::Held(None) => {
            anyhow::bail!("already running, pid not yet written; run `mogwai stop` first");
        }
    };

    let (read, write) = nix::unistd::pipe()?;
    match unsafe { nix::unistd::fork()? } {
        ForkResult::Parent { child } => {
            drop(write);
            // Leak, do NOT drop. The flock is tied to the open file description
            // the child shares through the fork, so `Flock::drop` issuing LOCK_UN
            // here would release the child's lock too and un-guard the daemon.
            // Closing the parent's fd on exit (without LOCK_UN) leaves the lock
            // held by the child; mem::forget gives exactly that.
            std::mem::forget(lock);
            await_ready(read, child, &args)
        }
        ForkResult::Child => {
            drop(read);
            nix::unistd::setsid()?;
            redirect_stdio_to_devnull()?;
            init_logging(&args.log_file)?;
            // From here on stderr points at /dev/null, so an `Err` propagating
            // out of `main` (malformed config, failed instrument validation, a
            // bind failure) would print to a discarded stream - while the
            // parent's failure message tells the operator to "see mogwai.log".
            // Mirror the failure into the tracing log (initialized just above)
            // before the process exits so that pointer is honest; `{:#}`
            // flattens the whole anyhow context chain into the one line.
            let result = run_runtime(args, Ready::Pipe(PipeReady::new(write)), Some(lock));
            if let Err(err) = &result {
                tracing::error!("daemon exiting on startup/runtime failure: {err:#}");
            }
            result
        }
    }
}

fn run_runtime(args: ResolvedServeArgs, ready: Ready, lock: Option<PidLock>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_async(args, ready, lock))
}

async fn serve_async(
    args: ResolvedServeArgs,
    mut ready: Ready,
    mut lock: Option<PidLock>,
) -> anyhow::Result<()> {
    let cfg = Config::load(args.config.clone())?;
    let profiles = Arc::new(build_instrument_profiles(&cfg)?);
    let sim = build_sim_clock(&cfg, now_ns())?;
    tracing::info!(
        sim_epoch_ns = cfg.sim_epoch_ns,
        wall_anchor_ns = sim.wall_anchor_ns,
        speed = cfg.speed,
        gap_cap_ms = cfg.gap_cap_ms,
        server_heartbeat_ms = cfg.server_heartbeat_ms,
        instruments = profiles.instrument_defs().len(),
        "config"
    );
    if !sim.is_identity() && cfg.gap_cap_ms != 0 {
        tracing::info!(
            gap_cap_ms = cfg.gap_cap_ms,
            "gap_cap_ms is ignored under simulated deadline pacing"
        );
    }
    // Derive the tape origin from the advertised clock, once, at boot. Every
    // generator anchors here, so the data timeline cannot drift past the clock
    // into the multi-year seek that emptied forward warmups (the #13 root).
    let data_origin_ns = sim.sim_ns(now_ns()).saturating_sub(cfg.backfill_horizon_ns);
    tracing::info!(
        data_origin_ns,
        backfill_horizon_ns = cfg.backfill_horizon_ns,
        "data origin"
    );

    // Fund the venue account before the first client connects: the adapter
    // pulls GET /account at connect and registers what it sees, so the seed
    // must be in the very first snapshot, not booked later.
    let mut funded: Vec<&String> = cfg.balances.keys().collect();
    funded.sort();
    tracing::info!(balances = ?funded, "account funding");
    warn_unfunded_quotes(&cfg, &profiles.instrument_defs());
    let tape_permits = build_tape_permits(&cfg);
    let (tapes, _tape_reaper) = tape::TapeRegistry::new(&cfg, tape_permits);
    let accounts = Arc::new(AccountRegistry::new(
        AccountTemplate {
            instruments: Arc::from(profiles.instrument_defs()),
            balances: cfg.balances.clone(),
        },
        cfg.max_accounts,
    ));
    let state = AppState {
        accounts: Arc::clone(&accounts),
        cfg: cfg.clone(),
        profiles,
        sim,
        data_origin_ns,
        tapes,
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/account", get(account))
        .route("/accounts", get(list_accounts))
        .route("/accounts/{id}", axum::routing::delete(delete_account))
        .route("/instruments", get(instruments))
        .route("/trades", get(trades))
        .route("/quotes", get(quotes))
        .route("/clock", get(clock))
        .route("/orders", post(submit_order_http))
        .route("/ws", get(ws_upgrade))
        .route("/control/divergence", post(arm_divergence))
        .with_state(state);

    if cfg.account_idle_timeout_ms != 0 {
        let reaper = Arc::clone(&accounts);
        let interval = cfg.account_reap_interval_ms;
        let idle_ns = cfg.account_idle_timeout_ms.saturating_mul(1_000_000);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(interval));
            loop {
                tick.tick().await;
                reaper.reap_idle(now_ns(), idle_ns);
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(args.addr).await?;
    tracing::info!(addr = %args.addr, "mogwai listening");
    if let Some(lock) = &mut lock {
        write_pid_into_locked_file(lock, std::process::id())?;
    }
    match &mut ready {
        Ready::Stdout => print_banner(args.addr, std::process::id(), &args.log_file),
        Ready::Pipe(pipe) => pipe.signal_ready()?,
    }
    serve_with_bounded_shutdown(listener, app).await?;
    if !args.foreground {
        ignore_error(std::fs::remove_file(&args.pid_file));
    }
    Ok(())
}

fn resolve_paths(args: ServeArgs) -> anyhow::Result<ResolvedServeArgs> {
    let cwd = std::env::current_dir()?;
    Ok(ResolvedServeArgs {
        config: Some(resolve_path(
            &cwd,
            args.config.unwrap_or_else(|| PathBuf::from("mogwai.toml")),
        )),
        addr: args.addr,
        log_file: resolve_path(&cwd, args.log_file),
        pid_file: resolve_path(&cwd, args.pid_file),
        foreground: args.foreground,
    })
}

fn resolve_path(cwd: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn init_logging(log_file: &Path) -> anyhow::Result<()> {
    // Logs go to a file, not the terminal: daemon mode redirects stdio to
    // /dev/null, and foreground wrappers can buffer stdout. The mutex writer
    // keeps each tracing event flushed and visible to a tailing operator.
    //
    // RUST_LOG is the one deliberate exception to the no-ambient-environment
    // rule that governs run knobs. It falls back to `mogwai=info`, matching the
    // binary target and this crate's tracing event target.
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mogwai=info".into()),
        )
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log_file))
        .try_init()
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    Ok(())
}

fn print_banner(addr: SocketAddr, pid: u32, log_file: &Path) {
    println!("mogwai serving on {addr}");
    println!("pid -> {pid}");
    println!("logs -> {}", log_file.display());
    println!("Listening.");
    let mut stdout = std::io::stdout();
    ignore_error(stdout.flush());
}

fn ignore_error<T, E>(result: Result<T, E>) {
    if let Err(err) = result {
        let _ignored = err;
    }
}

fn await_ready(read: OwnedFd, child: Pid, args: &ResolvedServeArgs) -> anyhow::Result<()> {
    let ready = read_ready_byte(&read, READY_TIMEOUT)?;
    drop(read);
    match ready {
        ReadyByte::Ready => {
            print_banner(args.addr, child.as_raw() as u32, &args.log_file);
            Ok(())
        }
        ReadyByte::Failed | ReadyByte::Eof => {
            ignore_error(std::fs::remove_file(&args.pid_file));
            anyhow::bail!("daemon failed to start; see {}", args.log_file.display());
        }
        ReadyByte::Timeout => {
            ignore_error(kill(child, Signal::SIGKILL));
            ignore_error(std::fs::remove_file(&args.pid_file));
            anyhow::bail!(
                "daemon did not signal readiness within {:?}; see {}",
                READY_TIMEOUT,
                args.log_file.display()
            );
        }
    }
}

enum ReadyByte {
    Ready,
    Failed,
    Eof,
    Timeout,
}

fn read_ready_byte(read: &OwnedFd, timeout: Duration) -> anyhow::Result<ReadyByte> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(ReadyByte::Timeout);
        }
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut pollfd = nix::libc::pollfd {
            fd: read.as_raw_fd(),
            events: nix::libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { nix::libc::poll(&mut pollfd, 1, timeout_ms) };
        if rc == 0 {
            return Ok(ReadyByte::Timeout);
        }
        if rc < 0 {
            let errno = Errno::last();
            if errno == Errno::EINTR {
                continue;
            }
            return Err(errno.into());
        }
        if pollfd.revents & (nix::libc::POLLIN | nix::libc::POLLHUP | nix::libc::POLLERR) == 0 {
            continue;
        }
        let mut buf = [0u8; 1];
        match nix::unistd::read(read, &mut buf) {
            Ok(1) if buf[0] == 0 => return Ok(ReadyByte::Ready),
            Ok(0) => return Ok(ReadyByte::Eof),
            Ok(_) => return Ok(ReadyByte::Failed),
            Err(Errno::EINTR) => continue,
            Err(errno) => return Err(errno.into()),
        }
    }
}

fn redirect_stdio_to_devnull() -> anyhow::Result<()> {
    let devnull = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    nix::unistd::dup2_stdin(&devnull)?;
    nix::unistd::dup2_stdout(&devnull)?;
    nix::unistd::dup2_stderr(&devnull)?;
    Ok(())
}

fn acquire_pid_lock(pid_file: &Path) -> anyhow::Result<PidLockStatus> {
    for _ in 0..PID_LOCK_ACQUIRE_ATTEMPTS {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(pid_file)?;
        match try_lock_pid_file(file)? {
            LockAttempt::Acquired(mut lock) => {
                // Guard against the flock-on-unlinked-pidfile race (S7): between
                // our open and our lock, a concurrent `stop` (or a prior daemon's
                // own cleanup) may have unlinked this inode and freed the path. We
                // would then hold an exclusive lock on a ghost file no later opener
                // can see, so a second `serve` could create a fresh inode at the
                // same path and start a rival daemon while `stop` sees neither. If
                // the path no longer names the inode we locked, drop the ghost lock
                // and retry from a clean open so our lock guards the live file.
                if !locked_inode_still_at_path(&lock, pid_file)? {
                    drop(lock);
                    continue;
                }
                clear_pid_file(&mut lock)?;
                return Ok(PidLockStatus::Acquired(lock));
            }
            LockAttempt::Held(mut file) => {
                return Ok(PidLockStatus::Held(read_pid_from_file(&mut file)?));
            }
        }
    }
    anyhow::bail!(
        "could not acquire a stable PID-file lock after {PID_LOCK_ACQUIRE_ATTEMPTS} attempts; \
         the pid file is being unlinked concurrently"
    )
}

/// True when `pid_file` still names the exact inode `lock` holds. Comparing the
/// locked fd's `(dev, ino)` (an fstat) against the path's (a stat) is what tells
/// a live lock apart from a lock on an inode already unlinked from the path - the
/// core of closing the S7 flock/unlink race on both the acquire and the unlink
/// sides. A missing path is trivially "not the same inode".
fn locked_inode_still_at_path(lock: &PidLock, pid_file: &Path) -> anyhow::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let locked = lock.0.metadata()?;
    match std::fs::metadata(pid_file) {
        Ok(path_meta) => Ok(path_meta.dev() == locked.dev() && path_meta.ino() == locked.ino()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Unlink `pid_file` only while `lock` proves we hold it AND the path still names
/// that exact inode. Removing under the held lock is what makes it safe: a
/// concurrent `serve` that already replaced the file has locked a different inode,
/// so the mismatch leaves its live pidfile untouched rather than deleting it out
/// from under a running rival (S7). Returns whether the unlink happened.
fn remove_pid_file_if_owned(lock: &PidLock, pid_file: &Path) -> bool {
    matches!(locked_inode_still_at_path(lock, pid_file), Ok(true))
        && std::fs::remove_file(pid_file).is_ok()
}

fn try_lock_pid_file(file: File) -> anyhow::Result<LockAttempt> {
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(LockAttempt::Acquired(PidLock(lock))),
        Err((file, errno)) if is_lock_held(errno) => Ok(LockAttempt::Held(file)),
        Err((_file, errno)) => Err(errno.into()),
    }
}

fn is_lock_held(errno: Errno) -> bool {
    errno == Errno::EWOULDBLOCK || errno == Errno::EAGAIN
}

fn clear_pid_file(lock: &mut PidLock) -> anyhow::Result<()> {
    lock.0.set_len(0)?;
    lock.0.seek(SeekFrom::Start(0))?;
    Ok(())
}

fn write_pid_into_locked_file(lock: &mut PidLock, pid: u32) -> anyhow::Result<()> {
    clear_pid_file(lock)?;
    writeln!(lock.0, "{pid}")?;
    lock.0.sync_data()?;
    Ok(())
}

fn read_pid_from_file(file: &mut File) -> anyhow::Result<Option<i32>> {
    file.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let pid = text.parse::<i32>()?;
    if pid <= 0 {
        anyhow::bail!("PID file contains non-positive pid {pid}");
    }
    Ok(Some(pid))
}

fn open_existing_pid_file(pid_file: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).open(pid_file)
}

fn stop(args: StopArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let pid_file = resolve_path(&cwd, args.pid_file);
    let file = match open_existing_pid_file(&pid_file) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("no daemon running (no {})", pid_file.display());
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    match try_lock_pid_file(file)? {
        LockAttempt::Acquired(lock) => {
            // Stale file, no daemon. Unlink it under the lock and only if the path
            // still names the inode we locked, so a `serve` that raced in and
            // replaced the file keeps its own live pidfile (S7).
            if remove_pid_file_if_owned(&lock, &pid_file) {
                println!("no daemon running (removed stale {})", pid_file.display());
            } else {
                println!("no daemon running (no {})", pid_file.display());
            }
            Ok(())
        }
        LockAttempt::Held(mut file) => {
            let pid = read_pid_when_ready(&mut file, STOP_TIMEOUT)?;
            signal_pid(pid, Signal::SIGTERM)?;
            match wait_for_lock_release(file, STOP_TIMEOUT)? {
                WaitLock::Released(lock) => {
                    remove_pid_file_if_owned(&lock, &pid_file);
                    Ok(())
                }
                WaitLock::StillHeld(file) => {
                    signal_pid(pid, Signal::SIGKILL)?;
                    match wait_for_lock_release(file, STOP_KILL_GRACE)? {
                        WaitLock::Released(lock) => {
                            remove_pid_file_if_owned(&lock, &pid_file);
                            Ok(())
                        }
                        WaitLock::StillHeld(_file) => {
                            anyhow::bail!(
                                "daemon pid {pid} did not exit after SIGTERM and SIGKILL"
                            );
                        }
                    }
                }
            }
        }
    }
}

fn read_pid_when_ready(file: &mut File, timeout: Duration) -> anyhow::Result<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = read_pid_from_file(file)? {
            return Ok(pid);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("daemon did not write its PID within {timeout:?}");
        }
        std::thread::sleep(PID_POLL_INTERVAL);
    }
}

fn wait_for_lock_release(file: File, timeout: Duration) -> anyhow::Result<WaitLock> {
    let deadline = Instant::now() + timeout;
    let mut file = file;
    loop {
        match try_lock_pid_file(file)? {
            // Hand the acquired lock back so the caller's verified unlink runs
            // while the lock is still held - removing the pidfile under the lock
            // is what keeps a fresh `serve` from opening the path, locking behind
            // us, and being torn from its own live pidfile (S7).
            LockAttempt::Acquired(lock) => return Ok(WaitLock::Released(lock)),
            LockAttempt::Held(returned) => {
                file = returned;
                if Instant::now() >= deadline {
                    return Ok(WaitLock::StillHeld(file));
                }
                std::thread::sleep(PID_POLL_INTERVAL);
            }
        }
    }
}

fn signal_pid(pid: i32, signal: Signal) -> anyhow::Result<()> {
    match kill(Pid::from_raw(pid), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(errno) => Err(errno.into()),
    }
}

async fn serve_with_bounded_shutdown(
    listener: tokio::net::TcpListener,
    app: Router,
) -> anyhow::Result<()> {
    tokio::spawn(async {
        shutdown_signal().await;
        tokio::time::sleep(SHUTDOWN_GRACE).await;
        std::process::exit(0);
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(e) => {
                tracing::warn!(%e, "failed to install SIGTERM handler");
                ignore_error(tokio::signal::ctrl_c().await);
                return;
            }
        };
    tokio::select! {
        _ = terminate.recv() => {}
        result = tokio::signal::ctrl_c() => {
            if let Err(e) = result {
                tracing::warn!(%e, "failed while waiting for Ctrl-C");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::Ticket;
    use crate::admission::{
        CLOSE_ADMISSION_OVERLOAD, CLOSE_GRACE, ExecLanes, HeldFrame, LaneReceivers, Outbound,
        OutboundFrame,
    };
    use crate::config::*;
    use crate::http::*;
    use crate::tape::{RegimeKey, TapeKey, TapeSpawn};
    use crate::ws::*;
    use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
    use futures_util::{SinkExt, StreamExt};
    use mogwai_data::TickEvent;
    use mogwai_protocol::{
        ClientMessage, MAX_HISTORY_LIMIT, MarketRegime, OrderType, ServerMessage, Side, SimClock,
        SubmitOrder, SubscriptionIssue, TimeInForce, TradeTick,
    };
    use rust_decimal::Decimal;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

    // A fixed boot-derived origin for the synthesis tests: production derives it
    // from the clock at boot, but these tests pin a concrete instant so a
    // from-origin page and its resume cursor are reproducible.
    const TEST_DATA_ORIGIN: u64 = 1_700_438_400_000_000_000;

    fn default_profiles() -> Arc<source::InstrumentProfiles> {
        Arc::new(source::InstrumentProfiles::defaults())
    }

    /// Lanes for a directly-spawned replay, WITH their receivers: a lane whose
    /// receiver was dropped refuses every send, which would make a test that
    /// asserts on a diagnostic assert on nothing.
    fn replay_lanes() -> (ExecLanes, LaneReceivers) {
        ExecLanes::detached()
    }

    /// The one diagnostic a replay thread may emit, decoded off the priority
    /// lane.
    async fn next_priority_frame(rx: &mut LaneReceivers) -> ServerMessage {
        let frame = rx
            .prio_rx
            .recv()
            .await
            .expect("a diagnostic frame, not silence");
        let Outbound::Frame(frame) = frame else {
            panic!("expected a frame, got a close")
        };
        serde_json::from_str(&frame.payload).expect("a decodable diagnostic")
    }

    fn state() -> AppState {
        let profiles = default_profiles();
        let accounts = Arc::new(AccountRegistry::new(
            AccountTemplate {
                instruments: Arc::from(profiles.instrument_defs()),
                balances: std::collections::HashMap::new(),
            },
            4096,
        ));
        AppState {
            accounts,
            cfg: Config {
                sim_epoch_ns: 0,
                speed: 0.0,
                gap_cap_ms: 0,
                server_heartbeat_ms: 0,
                backfill_horizon_ns: 86_400_000_000_000,
                max_concurrent_tapes: 1024,
                instruments: Vec::new(),
                ..Config::default()
            },
            profiles,
            sim: SimClock::identity(),
            // Coherent by construction, matching the boot formula: identity sim-now
            // is wall-now, so the tape origin sits 24h behind. A handler test that
            // needs a different floor overrides `data_origin_ns` after building.
            data_origin_ns: now_ns().saturating_sub(86_400_000_000_000),
            tapes: crate::tape::TapeRegistry::for_tests(
                &Config::default(),
                Arc::new(tokio::sync::Semaphore::new(1024)),
            ),
        }
    }

    /// Build a socket-test state with a live tape reaper. `state()` predates
    /// tapes and intentionally uses the no-runtime constructor for HTTP tests;
    /// these tests need the production lifetime path as their witness.
    fn tape_state(cfg: Config) -> AppState {
        tape_state_with(cfg, default_profiles())
    }

    /// The default catalog lists one instrument, so a test that needs several
    /// SYMBOLS (not several accounts) clones that one profile under new names.
    /// The generated bytes differ per symbol because the seed is derived from
    /// the symbol, which is exactly what makes them distinct tapes.
    fn symbol_profiles(symbols: &[&str]) -> Arc<source::InstrumentProfiles> {
        let defaults = source::InstrumentProfiles::defaults();
        let template = defaults.get("BTCUSDT").expect("the default instrument");
        let profiles = symbols
            .iter()
            .map(|symbol| {
                let mut def = template.def.clone();
                def.symbol = (*symbol).into();
                source::InstrumentProfile::new(
                    def,
                    template.scalars.clone(),
                    template.session.clone(),
                )
            })
            .collect();
        Arc::new(source::InstrumentProfiles::from_profiles(profiles))
    }

    fn tape_state_with(mut cfg: Config, profiles: Arc<source::InstrumentProfiles>) -> AppState {
        cfg.server_heartbeat_ms = 0;
        let (tapes, _reaper) = crate::tape::TapeRegistry::new(&cfg, build_tape_permits(&cfg));
        AppState {
            accounts: Arc::new(AccountRegistry::new(
                AccountTemplate {
                    instruments: Arc::from(profiles.instrument_defs()),
                    balances: Default::default(),
                },
                4096,
            )),
            profiles,
            sim: SimClock::identity(),
            data_origin_ns: now_ns().saturating_sub(cfg.backfill_horizon_ns),
            cfg,
            tapes,
        }
    }

    async fn tape_socket(
        addr: std::net::SocketAddr,
        account: &str,
    ) -> WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>> {
        connect_async(format!("ws://{addr}/ws?account={account}"))
            .await
            .expect("connect async websocket")
            .0
    }

    async fn subscribe_socket(
        socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
        symbol: &str,
        generation: u64,
        start_ts: Option<u64>,
        regime: Option<MarketRegime>,
    ) {
        let text = serde_json::to_string(&ClientMessage::Subscribe {
            subscriptions: vec![mogwai_protocol::SubscriptionRequest {
                generation,
                symbol: symbol.into(),
                start_ts,
                regime,
            }],
        })
        .expect("encode subscribe");
        socket
            .send(tungstenite::Message::Text(text.into()))
            .await
            .expect("send subscribe");
    }

    async fn next_trade_async(
        socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    ) -> TradeTick {
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(frame) = socket.next().await {
                let frame = frame.expect("read websocket frame");
                if let tungstenite::Message::Text(text) = frame
                    && let ServerMessage::Trade(tick) =
                        serde_json::from_str(&text).expect("decode server frame")
                {
                    return tick;
                }
            }
            panic!("socket ended before a trade")
        })
        .await
        .expect("trade arrives promptly")
    }

    async fn trade_sequence(
        socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
        n: usize,
    ) -> Vec<TradeTick> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            out.push(next_trade_async(socket).await);
        }
        out
    }

    async fn wait_for_tapes(tapes: &crate::tape::TapeRegistry, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while tapes.live_tapes() != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("tape count reaches its expected value");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fanout_scaling_measurement() {
        fn proc_measurement() -> (u64, u64) {
            let status = std::fs::read_to_string("/proc/self/status").expect("read proc status");
            let threads = status
                .lines()
                .find_map(|line| line.strip_prefix("Threads:"))
                .expect("Threads field")
                .trim()
                .parse()
                .expect("thread count");
            let stat = std::fs::read_to_string("/proc/self/stat").expect("read proc stat");
            let fields: Vec<_> = stat
                .rsplit_once(") ")
                .expect("stat comm")
                .1
                .split_whitespace()
                .collect();
            (
                threads,
                fields[11].parse::<u64>().expect("utime")
                    + fields[12].parse::<u64>().expect("stime"),
            )
        }
        async fn sample(n: usize) -> (u64, u64, Vec<usize>) {
            let cfg = Config {
                speed: 100.0,
                gap_cap_ms: 1,
                ..Config::default()
            };
            let addr = serve_ws(tape_state(cfg)).await;
            let (before_threads, before_cpu) = proc_measurement();
            let mut readers = Vec::new();
            for i in 0..n {
                let mut socket = tape_socket(addr, &format!("M-{i}")).await;
                subscribe_socket(&mut socket, "BTCUSDT", 1, None, None).await;
                readers.push(tokio::spawn(async move {
                    let end = tokio::time::Instant::now() + Duration::from_millis(1_000);
                    let mut frames = 0;
                    while tokio::time::Instant::now() < end {
                        let _ = next_trade_async(&mut socket).await;
                        frames += 1;
                    }
                    frames
                }));
            }
            let frames = futures_util::future::join_all(readers)
                .await
                .into_iter()
                .map(|x| x.expect("reader task"))
                .collect();
            let (threads, cpu) = proc_measurement();
            (
                threads.saturating_sub(before_threads),
                cpu.saturating_sub(before_cpu),
                frames,
            )
        }
        let one = sample(1).await;
        let many = sample(128).await;
        eprintln!(
            "fanout scaling: sessions threads cpu_ticks frames\n1 {} {} {:?}\n128 {} {} {:?}",
            one.0, one.1, one.2, many.0, many.1, many.2
        );
        assert!(one.2.iter().all(|n| *n > 0));
        assert!(many.2.iter().all(|n| *n > 0));
        // The acceptance threshold this instrument exists to gate: streaming
        // 128 sessions off one symbol must cost the same THREADS as streaming
        // one, because they share a tape. The CPU columns are printed rather
        // than asserted - a jiffy is 10 ms, so a one-second window has too
        // little resolution at N = 1 for a ratio to mean anything - and the
        // per-session frame counts above prove every session was actually fed.
        assert!(
            many.0 <= one.0 + 4,
            "128 sessions cost {} attributable threads against {} for one; \
             the tape is not being shared",
            many.0,
            one.0
        );
    }

    #[tokio::test]
    async fn two_clean_subscribers_share_one_tape() {
        let state = tape_state(Config {
            speed: 1.0,
            gap_cap_ms: 50,
            ..Config::default()
        });
        let tapes = Arc::clone(&state.tapes);
        let addr = serve_ws(state).await;
        let mut a = tape_socket(addr, "CLEAN-A").await;
        let mut b = tape_socket(addr, "CLEAN-B").await;
        subscribe_socket(&mut a, "BTCUSDT", 1, None, None).await;
        let _ = next_trade_async(&mut a).await;
        subscribe_socket(&mut b, "BTCUSDT", 1, None, None).await;
        wait_for_tapes(&tapes, 1).await;
        assert_eq!(
            trade_sequence(&mut a, 8)
                .await
                .into_iter()
                .map(|tick| serde_json::to_string(&tick).expect("serialize tick"))
                .collect::<Vec<_>>(),
            trade_sequence(&mut b, 8)
                .await
                .into_iter()
                .map(|tick| serde_json::to_string(&tick).expect("serialize tick"))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_regimed_subscriber_gets_its_own_tape() {
        let state = tape_state(Config {
            speed: 1.0,
            gap_cap_ms: 50,
            ..Config::default()
        });
        let tapes = Arc::clone(&state.tapes);
        let addr = serve_ws(state).await;
        let mut clean = tape_socket(addr, "REGIME-CLEAN").await;
        let mut drought = tape_socket(addr, "REGIME-DROUGHT").await;
        subscribe_socket(&mut clean, "BTCUSDT", 1, None, None).await;
        subscribe_socket(
            &mut drought,
            "BTCUSDT",
            1,
            None,
            Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
        )
        .await;
        wait_for_tapes(&tapes, 2).await;
        let _ = next_trade_async(&mut clean).await;
        let clean_frames = trade_sequence(&mut clean, 8).await;
        assert_ne!(
            clean_frames
                .iter()
                .map(|tick| serde_json::to_string(tick).expect("serialize tick"))
                .collect::<Vec<_>>(),
            trade_sequence(&mut drought, 8)
                .await
                .iter()
                .map(|tick| serde_json::to_string(tick).expect("serialize tick"))
                .collect::<Vec<_>>()
        );
        assert!(
            clean_frames
                .windows(2)
                .all(|pair| pair[0].ts_event < pair[1].ts_event),
            "the regime on the second tape must not perturb the clean tape"
        );
    }

    #[tokio::test]
    async fn identical_regimes_share_one_tape() {
        let state = tape_state(Config {
            speed: 0.0,
            ..Config::default()
        });
        let tapes = Arc::clone(&state.tapes);
        let addr = serve_ws(state).await;
        let regime = MarketRegime::VolStorm { vol_mult: 2.0 };
        let mut a = tape_socket(addr, "SAME-A").await;
        let mut b = tape_socket(addr, "SAME-B").await;
        subscribe_socket(&mut a, "BTCUSDT", 1, None, Some(regime)).await;
        subscribe_socket(&mut b, "BTCUSDT", 1, None, Some(regime)).await;
        wait_for_tapes(&tapes, 1).await;
        let _ = next_trade_async(&mut a).await;
        let _ = next_trade_async(&mut b).await;
    }

    #[tokio::test]
    async fn tape_is_reaped_when_its_last_subscriber_leaves() {
        let state = tape_state(Config {
            speed: 1.0,
            gap_cap_ms: 10_000,
            ..Config::default()
        });
        let tapes = Arc::clone(&state.tapes);
        let addr = serve_ws(state).await;
        tokio::task::spawn_blocking(|| ())
            .await
            .expect("warm the reaper's blocking pool");
        let before = std::fs::read_to_string("/proc/self/status").expect("status");
        let before_threads = before
            .lines()
            .find(|line| line.starts_with("Threads:"))
            .map(str::to_owned);
        let mut socket = tape_socket(addr, "REAP").await;
        subscribe_socket(&mut socket, "BTCUSDT", 1, None, None).await;
        wait_for_tapes(&tapes, 1).await;
        socket
            .send(tungstenite::Message::Text(
                serde_json::to_string(&ClientMessage::Unsubscribe {
                    symbols: vec!["BTCUSDT".into()],
                })
                .expect("unsubscribe")
                .into(),
            ))
            .await
            .expect("send unsubscribe");
        wait_for_tapes(&tapes, 0).await;
        let after_threads = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let threads = std::fs::read_to_string("/proc/self/status")
                    .expect("status")
                    .lines()
                    .find(|line| line.starts_with("Threads:"))
                    .map(str::to_owned);
                if threads == before_threads {
                    return threads;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("tape reaper joins its thread");
        assert_eq!(
            before_threads, after_threads,
            "the last tape's generator thread must be joined"
        );
    }

    #[tokio::test]
    async fn resubscribe_resumes_past_the_predecessors_last_tick() {
        let addr = serve_ws(tape_state(Config {
            speed: 0.0,
            ..Config::default()
        }))
        .await;
        let mut socket = tape_socket(addr, "RESUME").await;
        subscribe_socket(&mut socket, "BTCUSDT", 1, None, None).await;
        let first = trade_sequence(&mut socket, 8).await;
        let last = first.last().expect("frames").ts_event;
        subscribe_socket(&mut socket, "BTCUSDT", 2, None, None).await;
        let second = trade_sequence(&mut socket, 8).await;
        assert!(
            second.iter().all(|tick| tick.ts_event > last),
            "resubscribe repeats or regresses past {last}: {second:?}"
        );
    }

    #[tokio::test]
    async fn a_lagged_subscriber_is_a_venue_fault_that_kills_the_connection() {
        let cfg = Config {
            fanout_depth: 2,
            ..Config::default()
        };
        let registry =
            crate::tape::TapeRegistry::for_tests(&cfg, Arc::new(tokio::sync::Semaphore::new(1)));
        let key = crate::tape::TapeKey {
            symbol: "BTCUSDT".into(),
            data_origin_ns: TEST_DATA_ORIGIN,
            regime: crate::tape::RegimeKey::Clean,
        };
        let anchor = registry.attach_inert(key.clone()).expect("anchor lease");
        anchor.tape.publish_for_test(1);
        let lease = registry.attach_inert(key).expect("lagging lease");

        // The lease's receiver remains undrained while the tape overwrites its
        // two-frame ring. This reaches broadcast lag directly, without relying
        // on how much a kernel websocket buffer happens to absorb.
        for ts_event in 2..=5 {
            anchor.tape.publish_for_test(ts_event);
        }

        let (lanes, mut receivers) = replay_lanes();
        let (tx, mut rx) = mpsc::channel(1);
        let task = spawn_fanout(FanoutSpawn {
            symbol: "BTCUSDT".into(),
            generation: 7,
            lease,
            target: None,
            regime: None,
            profiles: default_profiles(),
            data_origin: TEST_DATA_ORIGIN,
            tx,
            diag_ticket: Some(lanes.reserve_promise().expect("diagnostic ticket")),
            lanes,
            cancel: Arc::new(AtomicBool::new(false)),
            cancel_wake: Arc::new(tokio::sync::Notify::new()),
            last_sent_ts: Arc::new(AtomicU64::new(NO_TICK_SENT)),
            sim: SimClock::identity(),
            speed: 0.0,
            gap_cap_ms: 0,
        });
        let ServerMessage::SubscriptionIssues { entries, .. } =
            tokio::time::timeout(Duration::from_secs(1), next_priority_frame(&mut receivers))
                .await
                .expect("FeedLagged diagnostic")
        else {
            panic!("expected SubscriptionIssues")
        };
        assert!(matches!(
            entries.as_slice(),
            [mogwai_protocol::SubscriptionOutcome {
                generation: 7,
                symbol,
                issue: SubscriptionIssue::FeedLagged { skipped },
            }] if symbol == "BTCUSDT" && *skipped > 0
        ));
        // The load-bearing half: a turned-over ring is a VENUE FAULT, not a
        // recoverable refusal, so the connection dies rather than streaming on
        // past an unarmed hole. Without this the diagnostic alone would let a
        // forward-validation run continue against silently-missing ticks.
        let close = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("close frame")
            .expect("writer channel open");
        let Outbound::Close(spec) = close else {
            panic!("expected a close, got a frame")
        };
        assert_eq!(spec.code, crate::admission::CLOSE_VENUE_FAULT);
        assert!(
            spec.reason.contains("venue fault") && spec.reason.contains("BTCUSDT"),
            "the close must name the fault and the symbol: {}",
            spec.reason
        );
        task.await.expect("fanout exits after the venue fault");
    }

    #[tokio::test]
    async fn tape_capacity_refuses_a_new_tape_not_a_new_subscriber() {
        let state = tape_state(Config {
            speed: 0.0,
            max_concurrent_tapes: 1,
            ..Config::default()
        });
        let tapes = Arc::clone(&state.tapes);
        let addr = serve_ws(state).await;
        let mut a = tape_socket(addr, "CAP-A").await;
        let mut same = tape_socket(addr, "CAP-SAME").await;
        let mut other = tape_socket(addr, "CAP-OTHER").await;
        subscribe_socket(&mut a, "BTCUSDT", 1, None, None).await;
        subscribe_socket(&mut same, "BTCUSDT", 1, None, None).await;
        let _ = next_trade_async(&mut same).await;
        wait_for_tapes(&tapes, 1).await;
        subscribe_socket(
            &mut other,
            "BTCUSDT",
            1,
            None,
            Some(MarketRegime::VolStorm { vol_mult: 2.0 }),
        )
        .await;
        let (issues, _, _) = tokio::time::timeout(Duration::from_secs(3), async {
            read_until_subscription_issues_async(&mut other).await
        })
        .await
        .expect("capacity reply");
        assert!(
            issues
                .iter()
                .any(|entry| matches!(entry.issue, SubscriptionIssue::ReplayCapacity))
        );
        a.send(tungstenite::Message::Text(
            serde_json::to_string(&ClientMessage::Unsubscribe {
                symbols: vec!["BTCUSDT".into()],
            })
            .expect("unsubscribe")
            .into(),
        ))
        .await
        .expect("unsubscribe");
        drop(same);
        wait_for_tapes(&tapes, 0).await;
        subscribe_socket(
            &mut other,
            "BTCUSDT",
            2,
            None,
            Some(MarketRegime::VolStorm { vol_mult: 2.0 }),
        )
        .await;
        assert!(next_trade_async(&mut other).await.ts_event > 0);
    }

    #[tokio::test]
    async fn first_subscriber_to_a_new_tape_receives_frames() {
        for start_ts in [None, Some(now_ns().saturating_sub(1_000_000_000))] {
            let addr = serve_ws(tape_state(Config {
                speed: 0.0,
                ..Config::default()
            }))
            .await;
            let mut socket = tape_socket(addr, "FIRST").await;
            subscribe_socket(&mut socket, "BTCUSDT", 1, start_ts, None).await;
            let frames = trade_sequence(&mut socket, 8).await;
            assert!(
                frames
                    .windows(2)
                    .all(|pair| pair[0].ts_event < pair[1].ts_event)
            );
        }
    }

    #[tokio::test]
    async fn unsubscribe_returns_promptly_on_an_idle_tape() {
        let state = tape_state(Config {
            speed: 1.0,
            gap_cap_ms: 10_000,
            ..Config::default()
        });
        let tapes = Arc::clone(&state.tapes);
        let addr = serve_ws(state).await;
        let mut socket = tape_socket(addr, "IDLE").await;
        subscribe_socket(&mut socket, "BTCUSDT", 1, None, None).await;
        wait_for_tapes(&tapes, 1).await;
        let started = Instant::now();
        socket
            .send(tungstenite::Message::Text(
                serde_json::to_string(&ClientMessage::Unsubscribe {
                    symbols: vec!["BTCUSDT".into()],
                })
                .expect("unsubscribe")
                .into(),
            ))
            .await
            .expect("unsubscribe");
        drop(socket);
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "idle-tape cancellation took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn zero_speed_tape_does_not_lag_a_reading_subscriber() {
        let cfg = Config {
            speed: 0.0,
            fanout_depth: 8,
            zero_speed_stall_ms: 20,
            ..Config::default()
        };
        let addr = serve_ws(tape_state(cfg)).await;
        let mut reading = tape_socket(addr, "ZERO-READING").await;
        let mut dead = tape_socket(addr, "ZERO-DEAD").await;
        subscribe_socket(&mut reading, "BTCUSDT", 1, None, None).await;
        subscribe_socket(&mut dead, "BTCUSDT", 1, None, None).await;
        let frames = trade_sequence(&mut reading, 64).await;
        assert!(
            frames
                .windows(2)
                .all(|pair| pair[0].ts_event < pair[1].ts_event)
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        drop(dead);
    }

    /// A subscriber joining a running tape with an explicit `start_ts` behind
    /// the cursor is handed the interval it missed from a PRIVATE checkpointed
    /// backfill, then the live broadcast - with the seam exact, because both
    /// phases are the same deterministic walk.
    #[tokio::test]
    async fn late_attacher_backfills_to_the_cursor() {
        let state = tape_state(Config {
            speed: 1_000.0,
            gap_cap_ms: 1,
            ..Config::default()
        });
        let tapes = Arc::clone(&state.tapes);
        let addr = serve_ws(state).await;
        let mut a = tape_socket(addr, "LATE-A").await;
        subscribe_socket(&mut a, "BTCUSDT", 1, None, None).await;
        let early = trade_sequence(&mut a, 8).await;
        let start = early.first().expect("frames").ts_event;

        let mut b = tape_socket(addr, "LATE-B").await;
        subscribe_socket(&mut b, "BTCUSDT", 1, Some(start), None).await;
        wait_for_tapes(&tapes, 1).await;
        let late = trade_sequence(&mut b, 16).await;
        assert!(
            late.windows(2)
                .all(|pair| pair[0].ts_event < pair[1].ts_event),
            "the backfill/live seam must stay strictly ascending: {late:?}"
        );
        // Whatever the two subscribers both saw must be byte-identical: the
        // private backfill reproduces the tape's own bytes.
        for early_tick in &early {
            if let Some(late_tick) = late.iter().find(|t| t.ts_event == early_tick.ts_event) {
                assert_eq!(
                    serde_json::to_string(early_tick).expect("serialize"),
                    serde_json::to_string(late_tick).expect("serialize"),
                );
            }
        }
    }

    /// The `target > attach_ts` case: a resubscribe whose resume floor was
    /// inherited from a predecessor on a DIFFERENT tape (here a regime change)
    /// must deliver nothing at or below `target - 1`. Filtering on the attach
    /// cursor alone would re-serve frames the predecessor already sent.
    #[tokio::test]
    async fn resume_floor_ahead_of_the_cursor_emits_no_duplicate() {
        let state = tape_state(Config {
            speed: 0.0,
            ..Config::default()
        });
        let addr = serve_ws(state).await;
        let mut socket = tape_socket(addr, "FLOOR").await;
        subscribe_socket(&mut socket, "BTCUSDT", 1, None, None).await;
        let first = trade_sequence(&mut socket, 8).await;
        let last = first.last().expect("frames").ts_event;
        // A regime change moves this subscription to a brand-new tape whose
        // cursor starts well behind the floor inherited from the clean one.
        subscribe_socket(
            &mut socket,
            "BTCUSDT",
            2,
            None,
            Some(MarketRegime::VolStorm { vol_mult: 2.0 }),
        )
        .await;
        let second = trade_sequence(&mut socket, 8).await;
        assert!(
            second.iter().all(|tick| tick.ts_event > last),
            "a floor ahead of the new tape's cursor still bounds delivery: \
             {second:?} must all exceed {last}"
        );
    }

    /// The per-connection subscription cap: a third distinct symbol is refused,
    /// a RESUBSCRIBE of an already-held symbol succeeds (the cap is checked
    /// after the quiesce, so a resubscribe is free), and another connection is
    /// unaffected.
    #[tokio::test]
    async fn per_connection_subscription_cap_refuses_the_overflow() {
        let symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];
        let state = tape_state_with(
            Config {
                speed: 0.0,
                max_subscriptions_per_connection: 2,
                ..Config::default()
            },
            symbol_profiles(&symbols),
        );
        let addr = serve_ws(state).await;
        let mut capped = tape_socket(addr, "CAP-CONN").await;
        for symbol in symbols.iter().take(2) {
            subscribe_socket(&mut capped, symbol, 1, None, None).await;
        }
        subscribe_socket(&mut capped, symbols[2], 1, None, None).await;
        let (issues, _, _) = tokio::time::timeout(
            Duration::from_secs(3),
            read_until_subscription_issues_async(&mut capped),
        )
        .await
        .expect("capacity reply");
        assert!(
            issues.iter().any(|entry| entry.symbol == symbols[2]
                && matches!(entry.issue, SubscriptionIssue::ReplayCapacity)),
            "the third distinct symbol is refused: {issues:?}"
        );
        // A resubscribe of a symbol already held replaces its predecessor
        // rather than being refused by it.
        subscribe_socket(&mut capped, symbols[0], 2, None, None).await;
        assert!(next_trade_async(&mut capped).await.ts_event > 0);
        // The cap is per connection, so a second socket is unaffected.
        let mut other = tape_socket(addr, "CAP-CONN-2").await;
        subscribe_socket(&mut other, symbols[2], 1, None, None).await;
        assert!(next_trade_async(&mut other).await.ts_event > 0);
    }

    async fn read_until_subscription_issues_async(
        socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    ) -> (Vec<mogwai_protocol::SubscriptionOutcome>, usize, usize) {
        while let Some(frame) = socket.next().await {
            if let tungstenite::Message::Text(text) = frame.expect("websocket frame")
                && let ServerMessage::SubscriptionIssues {
                    entries,
                    issues_total,
                    refusals_total,
                    ..
                } = serde_json::from_str(&text).expect("decode")
            {
                return (entries, issues_total, refusals_total);
            }
        }
        panic!("socket ended before subscription issues")
    }

    fn account_headers() -> axum::http::HeaderMap {
        account_headers_for("TEST-001")
    }

    fn account_headers_for(account: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            mogwai_protocol::ACCOUNT_HEADER,
            axum::http::HeaderValue::from_str(account).expect("valid header"),
        );
        headers
    }

    fn limit_order(id: &str) -> ClientMessage {
        ClientMessage::SubmitOrder(SubmitOrder {
            client_order_id: id.into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Decimal::ONE,
            price: Some(Decimal::from(100)),
            time_in_force: TimeInForce::Gtc,
        })
    }

    #[tokio::test]
    async fn two_accounts_have_independent_ledgers() {
        let app = state();
        let _events = submit_order_http(
            State(app.clone()),
            account_headers_for("ACC-A"),
            Json(limit_order("A-1")),
        )
        .await
        .expect("A order accepted");
        let Json(a) = account(State(app.clone()), account_headers_for("ACC-A"))
            .await
            .unwrap();
        let Json(b) = account(State(app), account_headers_for("ACC-B"))
            .await
            .unwrap();
        assert!(!a.positions.is_empty());
        assert!(b.positions.is_empty());
        assert!(b.balances.is_empty());
    }

    #[tokio::test]
    async fn an_order_is_invisible_to_another_accounts_query() {
        let app = state();
        let _events = submit_order_http(
            State(app.clone()),
            account_headers_for("ACC-A"),
            Json(limit_order("SAME")),
        )
        .await
        .unwrap();
        let Json(orders) = submit_order_http(
            State(app.clone()),
            account_headers_for("ACC-B"),
            Json(ClientMessage::QueryOrders {
                request_id: "Q1".into(),
                client_order_id: None,
                open_only: false,
            }),
        )
        .await
        .unwrap();
        let Json(fills) = submit_order_http(
            State(app),
            account_headers_for("ACC-B"),
            Json(ClientMessage::QueryFills {
                request_id: "Q2".into(),
                client_order_id: None,
            }),
        )
        .await
        .unwrap();
        assert!(
            matches!(&orders[0], ServerMessage::OrderStatusSnapshot(snapshot) if snapshot.orders.is_empty())
        );
        assert!(
            matches!(&fills[0], ServerMessage::FillSnapshot(snapshot) if snapshot.fills.is_empty())
        );
    }

    #[tokio::test]
    async fn same_client_order_id_in_two_accounts_both_accepted() {
        let app = state();
        for account_id in ["ACC-A", "ACC-B"] {
            let Json(events) = submit_order_http(
                State(app.clone()),
                account_headers_for(account_id),
                Json(limit_order("REUSED")),
            )
            .await
            .unwrap();
            assert!(matches!(events[0], ServerMessage::OrderAccepted { .. }));
        }
    }

    #[tokio::test]
    async fn cancel_across_accounts_is_refused() {
        let app = state();
        let _events = submit_order_http(
            State(app.clone()),
            account_headers_for("ACC-A"),
            Json(limit_order("A-ONLY")),
        )
        .await
        .unwrap();
        let Json(events) = submit_order_http(
            State(app),
            account_headers_for("ACC-B"),
            Json(ClientMessage::CancelOrder {
                client_order_id: "A-ONLY".into(),
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            events[0],
            ServerMessage::OrderCancelRejected { .. }
        ));
    }

    #[tokio::test]
    async fn missing_account_header_is_rejected() {
        let app = state();
        assert_eq!(
            account(State(app.clone()), axum::http::HeaderMap::new())
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            submit_order_http(
                State(app.clone()),
                axum::http::HeaderMap::new(),
                Json(limit_order("X"))
            )
            .await
            .unwrap_err(),
            StatusCode::BAD_REQUEST
        );
        let response = arm_divergence(
            State(app),
            axum::http::HeaderMap::new(),
            Json(mogwai_protocol::control::Divergence::ClearDivergences),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn ws_upgrade_without_the_account_param_is_rejected_before_handshake() {
        let addr = serve_ws(state()).await;
        let result =
            tokio::task::spawn_blocking(move || tungstenite::connect(format!("ws://{addr}/ws")))
                .await
                .expect("blocking websocket client");
        let err = result.expect_err("missing account query parameter must refuse the upgrade");
        match err {
            tungstenite::Error::Http(response) => {
                assert_eq!(response.status(), StatusCode::BAD_REQUEST);
                let body = response.body().as_ref().expect("error body");
                let body = std::str::from_utf8(body).expect("UTF-8 error body");
                assert!(body.contains(mogwai_protocol::ACCOUNT_QUERY_PARAM));
            }
            other => panic!("expected an HTTP handshake refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_permits_are_unchanged_by_account_scoping() {
        let app_state = state();
        let tapes = Arc::clone(&app_state.tapes);
        let addr = serve_ws(app_state).await;
        let held_permits = tokio::task::spawn_blocking(move || {
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], None),
            })
            .expect("encode subscribe");
            let (mut a, _) =
                tungstenite::connect(format!("ws://{addr}/ws?account=ACC-A")).expect("A connects");
            a.send(tungstenite::Message::Text(subscribe.clone().into()))
                .expect("A subscribes");
            read_until_trade(&mut a);
            let (mut b, _) =
                tungstenite::connect(format!("ws://{addr}/ws?account=ACC-B")).expect("B connects");
            b.send(tungstenite::Message::Text(subscribe.into()))
                .expect("B subscribes");
            read_until_trade(&mut b);
            tapes.live_tapes()
        })
        .await
        .expect("blocking websocket clients");
        assert_eq!(held_permits, 1, "clean subscriptions share one global tape");
    }

    #[tokio::test]
    async fn go_dark_is_scoped_to_the_arming_account() {
        let app = state();
        let response = arm_divergence(
            State(app.clone()),
            account_headers_for("ACC-A"),
            Json(mogwai_protocol::control::Divergence::GoDark { ms: 1_000 }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let a = app
            .accounts
            .get(&mogwai_protocol::AccountId::parse("ACC-A").unwrap())
            .unwrap();
        let b = app
            .accounts
            .acquire(
                &mogwai_protocol::AccountId::parse("ACC-B").unwrap(),
                now_ns(),
            )
            .unwrap();
        assert!(a.dark_until_ns.load(Ordering::Relaxed) > sim_now_ns(app.sim));
        assert_eq!(b.dark_until_ns.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn delay_acks_is_scoped_to_the_arming_account() {
        let app = state();
        let response = arm_divergence(
            State(app.clone()),
            account_headers_for("ACC-A"),
            Json(mogwai_protocol::control::Divergence::DelayAcks { ms: 1_000 }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let a = app
            .accounts
            .get(&mogwai_protocol::AccountId::parse("ACC-A").unwrap())
            .unwrap();
        let b = app
            .accounts
            .acquire(
                &mogwai_protocol::AccountId::parse("ACC-B").unwrap(),
                now_ns(),
            )
            .unwrap();
        assert_eq!(a.delay_ms.load(Ordering::Relaxed), 1_000);
        assert_eq!(b.delay_ms.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn teardown_during_an_in_flight_order_does_not_fill_a_phantom() {
        let app = state();
        let id = mogwai_protocol::AccountId::parse("ACC-A").unwrap();
        let slot = app.accounts.acquire(&id, now_ns()).unwrap();
        let engine = slot.engine.lock().await;
        assert!(app.accounts.destroy(&id));
        drop(engine);
        assert!(slot.tombstoned.load(Ordering::Relaxed));
        assert!(
            slot.engine
                .lock()
                .await
                .account_snapshot(now_ns())
                .positions
                .is_empty()
        );
    }

    #[test]
    fn malformed_account_header_is_rejected() {
        let app = state();
        assert!(account_from_headers(&app, &account_headers_for("bad/id")).is_err());
        assert_eq!(app.accounts.len(), 0);
    }

    #[tokio::test]
    async fn a_rejected_request_with_a_valid_id_creates_no_account() {
        let app = state();
        assert_eq!(
            submit_order_http(
                State(app.clone()),
                account_headers_for("ACC-A"),
                Json(ClientMessage::Subscribe {
                    subscriptions: Vec::new()
                })
            )
            .await
            .unwrap_err(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(app.accounts.len(), 0);
    }

    #[tokio::test]
    async fn teardown_destroys_the_account_and_its_state() {
        let app = state();
        let _events = submit_order_http(
            State(app.clone()),
            account_headers_for("ACC-A"),
            Json(limit_order("OLD")),
        )
        .await
        .unwrap();
        assert_eq!(
            delete_account(State(app.clone()), axum::extract::Path("ACC-A".into()))
                .await
                .unwrap(),
            StatusCode::OK
        );
        let Json(fresh) = account(State(app), account_headers_for("ACC-A"))
            .await
            .unwrap();
        assert!(fresh.positions.is_empty());
        assert!(fresh.balances.is_empty());
    }

    #[tokio::test]
    async fn teardown_of_unknown_account_is_404() {
        assert_eq!(
            delete_account(State(state()), axum::extract::Path("MISSING".into()))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn teardown_of_a_malformed_id_is_400() {
        assert_eq!(
            delete_account(State(state()), axum::extract::Path("bad/id".into()))
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn an_http_only_account_stays_alive_while_polling() {
        let app = state();
        for now in [20_u64, 40, 60] {
            let idle = mogwai_protocol::AccountId::parse(&format!("IDLE-{now}")).unwrap();
            app.accounts.acquire(&idle, 1).unwrap();
            let _snapshot = account(State(app.clone()), account_headers_for("POLL"))
                .await
                .unwrap();
            assert_eq!(app.accounts.reap_idle(now, 15), vec![idle.clone()]);
            assert!(
                app.accounts
                    .get(&mogwai_protocol::AccountId::parse("POLL").unwrap())
                    .is_some()
            );
        }
    }

    #[tokio::test]
    async fn instruments_match_the_configured_profiles() {
        // `/instruments` answers from the shared profiles, not from an engine:
        // with a registry there is no non-arbitrary account to ask for a
        // venue-global fact. The two lists must stay byte-identical.
        let app = state();
        let Json(listed) = instruments(State(app.clone())).await;
        assert_eq!(
            serde_json::to_string(&listed).unwrap(),
            serde_json::to_string(&app.profiles.instrument_defs()).unwrap()
        );
        assert_eq!(
            app.accounts.len(),
            0,
            "a venue-global listing must create no account"
        );
    }

    #[tokio::test]
    async fn an_order_into_a_destroyed_account_is_gone() {
        // A DELETE landing between identity resolution and the engine lock must
        // not answer 200: the caller would believe its order was booked into a
        // ledger nobody will ever read again.
        let app = state();
        let id = mogwai_protocol::AccountId::parse("ACC-A").unwrap();
        let slot = app.accounts.acquire(&id, now_ns()).unwrap();
        assert!(app.accounts.destroy(&id));
        let (lanes, _receivers) = ExecLanes::detached();
        assert!(matches!(
            process_order_cmd(limit_order("DOOMED"), &app, &slot, &lanes).await,
            OrderOutcome::Gone(_)
        ));
    }

    #[tokio::test]
    async fn teardown_closes_a_data_only_session() {
        // Deliberately data-only: a session that sent an order would also pass
        // under lazy discover-on-next-engine-access, which is the design this
        // test exists to forbid. A market-data-only socket makes no engine
        // access at all, so only the `closed` notification can reach it.
        let app = state();
        let accounts = Arc::clone(&app.accounts);
        let addr = serve_ws(app).await;
        let reason = tokio::task::spawn_blocking(move || {
            let (mut socket, _) =
                tungstenite::connect(format!("ws://{addr}/ws?account=DOOMED")).expect("ws connect");
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], None),
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send subscribe");
            read_until_trade(&mut socket);
            assert!(accounts.destroy(&mogwai_protocol::AccountId::parse("DOOMED").unwrap()));
            loop {
                let msg = socket.read().expect("read after teardown");
                let tungstenite::Message::Text(text) = msg else {
                    continue;
                };
                if let Ok(ServerMessage::ProtocolError { reason, .. }) =
                    serde_json::from_str::<ServerMessage>(&text)
                {
                    break reason;
                }
            }
        })
        .await
        .expect("blocking websocket client");
        assert!(reason.contains("destroyed"), "{reason}");
    }

    #[test]
    fn zero_reap_interval_is_a_config_error() {
        let err = validate_account_lifecycle(&Config {
            account_reap_interval_ms: 0,
            ..Config::default()
        })
        .expect_err("a zero reap interval has no sound reading")
        .to_string();
        assert!(err.contains("account_reap_interval_ms"), "{err}");
        assert!(validate_account_lifecycle(&Config::default()).is_ok());
    }

    #[test]
    fn sim_clock_config_rejects_two_knob_trap() {
        let cfg = Config {
            speed: 2.0,
            ..Config::default()
        };

        let err = build_sim_clock(&cfg, 123)
            .expect_err("data-only acceleration must be rejected")
            .to_string();

        assert!(err.contains("sim_epoch_ns must be set"));
    }

    #[test]
    fn sim_clock_config_builds_identity_and_accelerated_maps() {
        let mut cfg = Config::default();
        assert_eq!(
            build_sim_clock(&cfg, 123).expect("identity clock"),
            SimClock::identity()
        );

        cfg.speed = 0.0;
        assert_eq!(
            build_sim_clock(&cfg, 123).expect("legacy firehose keeps identity clock"),
            SimClock::identity()
        );

        cfg.sim_epoch_ns = 1_700_438_400_000_000_000;
        cfg.speed = 3_600.0;
        assert_eq!(
            build_sim_clock(&cfg, 123).expect("accelerated clock"),
            SimClock {
                sim_epoch_ns: 1_700_438_400_000_000_000,
                wall_anchor_ns: 123,
                speed: 3_600.0,
            }
        );

        cfg.speed = 0.0;
        assert!(build_sim_clock(&cfg, 123).is_err());
    }

    #[test]
    fn sim_clock_pinned_wall_anchor_survives_restarts() {
        // A pinned anchor puts every boot on the same affine axis: two boots at
        // different wall instants build the identical clock, so a venue restart
        // does not rewind sim-now to the epoch (the default boot anchor does).
        let cfg = Config {
            sim_epoch_ns: 1_700_438_400_000_000_000,
            wall_anchor_ns: 1_000,
            speed: 120.0,
            ..Config::default()
        };

        let first_boot = build_sim_clock(&cfg, 2_000).expect("first boot");
        let restart = build_sim_clock(&cfg, 900_000).expect("restart");
        assert_eq!(first_boot, restart);
        assert_eq!(first_boot.wall_anchor_ns, 1_000);

        // A pinned anchor in the future would freeze the venue at the epoch
        // until the wall catches up - refused as a misconfiguration.
        let err = build_sim_clock(&cfg, 500)
            .expect_err("future anchor refused")
            .to_string();
        assert!(err.contains("in the future"));

        // The anchor is meaningless without an epoch to anchor.
        let identity = Config {
            sim_epoch_ns: 0,
            wall_anchor_ns: 1_000,
            ..Config::default()
        };
        let err = build_sim_clock(&identity, 2_000)
            .expect_err("anchor without epoch refused")
            .to_string();
        assert!(err.contains("requires sim_epoch_ns"));
    }

    #[test]
    fn config_rejects_unknown_top_level_key() {
        // S20: a typo'd knob must be a hard error, not a silent fallback to the
        // field default - making config.md's "malformed file is a hard error"
        // promise literally true for the run knobs.
        let err = toml::from_str::<Config>("gap_cap_m = 0\n")
            .expect_err("an unknown top-level key is rejected")
            .to_string();
        assert!(
            err.contains("gap_cap_m") || err.contains("unknown"),
            "the error names the bad key: {err}"
        );
        // A correctly-spelled key still parses (missing keys keep their defaults).
        assert!(toml::from_str::<Config>("gap_cap_ms = 5\n").is_ok());
    }

    /// A minimal valid `[[instrument]]` table, mirroring reference/config.md's
    /// worked example. `{extra}` is spliced in so one template serves both the
    /// clean parse and the typo case.
    fn instrument_toml(extra: &str) -> String {
        format!(
            "[[instrument]]\n\
             symbol = \"EURUSD\"\n\
             base = \"EUR\"\n\
             quote = \"USD\"\n\
             price_precision = 4\n\
             size_precision = 8\n\
             price_increment = \"0.0001\"\n\
             size_increment = \"0.00000001\"\n\
             {extra}\n\
             [instrument.generator]\n\
             modal_tick = \"0.0001\"\n\
             price_decimals = 4\n\
             mean_duration_s = 7.194349711185499\n\
             size_round_frac = 0.20856767610054022\n\
             start_price = \"1.1000\"\n\
             typical_size = \"100000.0\"\n\
             vol_scalar = 0.00000005\n\
             \n\
             [instrument.session]\n\
             intensity_hour = [{ones24}]\n\
             vol_hour = [{ones24}]\n\
             dow_weight = [{ones7}]\n",
            extra = extra,
            ones24 = ["1.0"; 24].join(", "),
            ones7 = ["1.0"; 7].join(", "),
        )
    }

    #[test]
    fn config_rejects_unknown_instrument_key() {
        // The instrument half of S20. The def used to arrive via
        // `#[serde(flatten)]`, which serde cannot combine with
        // `deny_unknown_fields`, so every unknown key in the table was
        // swallowed: `price_precison = 4` silently ran the venue at the default
        // precision. The fields are spelled out on `ConfiguredInstrument` now,
        // so the typo is a hard error like any top-level knob.
        let clean = instrument_toml("");
        assert!(
            toml::from_str::<Config>(&clean).is_ok(),
            "the worked example from reference/config.md must still parse"
        );

        let err = toml::from_str::<Config>(&instrument_toml("price_precison = 4"))
            .expect_err("an unknown instrument key is rejected")
            .to_string();
        assert!(
            err.contains("price_precison") || err.contains("unknown"),
            "the error names the bad key: {err}"
        );
    }

    #[test]
    fn balances_default_funded_and_empty_table_unfunds() {
        // An absent [balances] table keeps the funded built-in default (the
        // committed mogwai.toml parity), while an explicitly EMPTY table is the
        // deliberate unfunded account.
        let absent: Config = toml::from_str("").expect("empty config");
        assert_eq!(absent.balances.get("USDT"), Some(&Decimal::from(1_000_000)));

        let empty: Config = toml::from_str("[balances]\n").expect("empty balances table");
        assert!(empty.balances.is_empty());

        let custom: Config =
            toml::from_str("[balances]\nUSDT = \"250000\"\nBTC = \"2.5\"\n").expect("custom");
        assert_eq!(custom.balances.get("USDT"), Some(&Decimal::from(250_000)));
        assert_eq!(
            custom.balances.get("BTC"),
            Some(&Decimal::new(25, 1)),
            "decimal strings parse exactly"
        );
    }

    #[test]
    fn balances_validation_refuses_negative_and_blank() {
        let mut cfg = Config {
            balances: std::collections::HashMap::from([("USDT".to_string(), Decimal::from(-1))]),
            ..Config::default()
        };
        let err = validate_balances(&cfg).expect_err("negative funding refused");
        assert!(err.to_string().contains("must not be negative"));

        cfg.balances = std::collections::HashMap::from([(" ".to_string(), Decimal::ONE)]);
        let err = validate_balances(&cfg).expect_err("blank currency refused");
        assert!(err.to_string().contains("must not be blank"));

        cfg.balances = std::collections::HashMap::from([("USDT".to_string(), Decimal::ZERO)]);
        validate_balances(&cfg).expect("zero funding is allowed");
    }

    /// The admission budgets are operator config, so a configured venue that
    /// could never admit anything must die at startup rather than answer every
    /// order with a capacity refusal. The floor is one boundary refusal - the
    /// single frame a malformed order produces - because below it even saying
    /// no is unaffordable.
    #[test]
    fn admission_limits_validation_refuses_a_venue_that_cannot_answer() {
        let defaults = Config::default();
        validate_admission_limits(&defaults).expect("the shipped defaults are valid");

        let mut cfg = Config {
            exec_held_budget_bytes: mogwai_protocol::sizing::BOUNDARY_REFUSAL_BYTES - 1,
            ..Config::default()
        };
        let err = validate_admission_limits(&cfg).expect_err("a sub-refusal budget is refused");
        assert!(err.to_string().contains("exec_held_budget_bytes"));

        cfg = Config {
            admission_lane_frames: 0,
            ..Config::default()
        };
        let err = validate_admission_limits(&cfg).expect_err("a zero-slot lane is refused");
        assert!(err.to_string().contains("admission_lane_frames"));

        cfg = Config {
            admission_promise_tickets: 0,
            ..Config::default()
        };
        let err = validate_admission_limits(&cfg).expect_err("a zero promise pool is refused");
        assert!(err.to_string().contains("admission_promise_tickets"));

        // The floor binds operator config only: a test that needs a refusal
        // reachable over a socket builds the value directly, which is what
        // `reservation_failure_leaves_engine_state_untouched` does.
        let limits = build_admission_limits(&Config::default());
        assert_eq!(
            limits.held_budget_bytes,
            crate::admission::EXEC_HELD_BUDGET_BYTES,
            "the config default IS the shipped constant"
        );
    }

    #[tokio::test]
    async fn clock_route_returns_stored_run_clock_and_tape_boundary() {
        let sim = SimClock {
            sim_epoch_ns: 10,
            wall_anchor_ns: 20,
            speed: 30.0,
        };
        let mut state = state();
        state.sim = sim;
        state.data_origin_ns = TEST_DATA_ORIGIN;

        let Json(returned) = clock(State(state)).await;

        // The affine map and the tape boundary both ride the one payload, so a
        // client gets sim-now and the floor from one round trip.
        assert_eq!(returned.sim, sim);
        assert_eq!(returned.data_origin_ns, TEST_DATA_ORIGIN);
        assert_eq!(returned.backfill_horizon_ns, 86_400_000_000_000);
        // `server_now_ns` is sim-now sampled in the handler; it must equal the
        // clock applied to some wall instant, so it sits at or above the epoch.
        assert!(returned.server_now_ns >= sim.sim_epoch_ns);
    }

    #[tokio::test]
    async fn http_orders_route_processes_order_commands() {
        let response = submit_order_http(
            State(state()),
            account_headers(),
            Json(ClientMessage::SubmitOrder(SubmitOrder {
                client_order_id: "HTTP1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: OrderType::Limit,
                quantity: "1".parse().expect("decimal"),
                price: Some("100".parse().expect("decimal")),
                time_in_force: TimeInForce::Gtc,
            })),
        )
        .await
        .expect("order accepted");

        assert!(matches!(response.0[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(response.0[1], ServerMessage::OrderFilled(_)));
        assert!(matches!(response.0[2], ServerMessage::AccountState(_)));
    }

    #[tokio::test]
    async fn http_orders_route_fills_a_priceless_market_order() {
        // A Nautilus MARKET order carries no price on the wire - mirroring that,
        // this submit omits `price` entirely. The engine has no book and only
        // knows how to fill "at the order's own price", so the route must stamp
        // one from the venue's own synthesized tape before the engine sees it;
        // unfixed, this rejects with "submit price required" instead of filling.
        let response = submit_order_http(
            State(state()),
            account_headers(),
            Json(ClientMessage::SubmitOrder(SubmitOrder {
                client_order_id: "HTTP-MKT1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: OrderType::Market,
                quantity: "1".parse().expect("decimal"),
                price: None,
                time_in_force: TimeInForce::Gtc,
            })),
        )
        .await
        .expect("order accepted");

        assert!(
            matches!(response.0[0], ServerMessage::OrderAccepted { .. }),
            "expected acceptance, got {:?}",
            response.0[0]
        );
        let ServerMessage::OrderFilled(fill) = &response.0[1] else {
            panic!("expected a fill, got {:?}", response.0[1]);
        };
        assert!(fill.last_px > Decimal::ZERO);
    }

    #[tokio::test]
    async fn priceless_market_order_reports_synthesis_failure_honestly() {
        // Push the origin 90 days behind sim-now: `current_price`'s checkpoint
        // index extends at most MAX_HISTORY_SEEK_TICKS per call and the
        // BoundedSeek drains at most the same again (~31 days of tape at the
        // committed cadence), so the price seek cannot reach sim-now and comes
        // back empty. Left alone, the engine rejects the still-price-less
        // order with "submit price required" - the wrong story for a client
        // that correctly sent a MARKET order with no price. The route must
        // reject with the venue's own synthesis-failure reason instead.
        let mut state = state();
        state.data_origin_ns = now_ns().saturating_sub(90 * 86_400_000_000_000);
        let response = submit_order_http(
            State(state),
            account_headers(),
            Json(ClientMessage::SubmitOrder(SubmitOrder {
                client_order_id: "HTTP-MKT-DEAD".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: OrderType::Market,
                quantity: "1".parse().expect("decimal"),
                price: None,
                time_in_force: TimeInForce::Gtc,
            })),
        )
        .await
        .expect("route answers");

        let ServerMessage::OrderRejected { reason, .. } = &response.0[0] else {
            panic!("expected a rejection, got {:?}", response.0[0]);
        };
        assert!(
            reason.contains("synthesize"),
            "the reject names the venue's synthesis failure: {reason}"
        );
        assert!(
            !reason.contains("submit price required"),
            "the engine's limit-order reason must not leak onto the market path: {reason}"
        );
    }

    #[tokio::test]
    async fn ws_route_reports_undecodable_frames_instead_of_dropping_them() {
        // A `Subscribe` missing its required `symbols` field used to vanish
        // silently - 0 frames back, socket left open - indistinguishable from a
        // healthy-but-idle feed. It must now come back as a `ProtocolError`.
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        let frame = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            socket
                .send(tungstenite::Message::Text(r#"{"type":"Subscribe"}"#.into()))
                .expect("send malformed subscribe");
            loop {
                match socket.read().expect("read ws frame") {
                    tungstenite::Message::Text(text) => {
                        return serde_json::from_str::<ServerMessage>(&text)
                            .expect("decode ServerMessage");
                    }
                    tungstenite::Message::Close(_) => panic!("socket closed with no reply"),
                    _ => continue,
                }
            }
        })
        .await
        .expect("blocking ws client");

        // Now named for what it is: a frame the venue could not decode is
        // admission truth, so it comes back attributed to the FRAME subject and
        // rides the priority lane, exempt from DelayAcks.
        assert!(
            matches!(
                frame,
                ServerMessage::AdmissionRejected {
                    subject: mogwai_protocol::AdmissionSubject::Frame,
                    ..
                }
            ),
            "expected an AdmissionRejected for the undecodable frame, got {frame:?}"
        );
    }

    /// MEASUREMENT: does an armed `DelayAcks` stop the venue READING?
    ///
    /// The exec delay pump USED to be a
    /// bounded channel (1024) whose producers - order-entry events and
    /// `ProtocolError` diagnostics alike - `.await`ed their send from inside the
    /// socket read loop. If the pump filled, those sends blocked and the session
    /// stopped reading client frames entirely. Admission control is the answer:
    /// the held lane is unbounded by channel capacity and bounded by a BYTE
    /// budget reserved before the engine mutates, so a read-loop send can never
    /// block, and diagnostics left the pump for the priority lane
    /// altogether. `reference/havoc.md` documents
    /// `DelayAcks` as holding outbound EXECUTION events by `ms`; it does not say
    /// the venue stops accepting commands. An output-latency fault that silently
    /// becomes an input-refusal fault is indistinguishable from the blackout
    /// `GoDark` models, in the component that exists to keep injected faults
    /// legible.
    ///
    /// The witness is a TRADE, not an order event. Market data is written to the
    /// writer channel directly by the replay thread and never touches the exec
    /// pump, so a trade arriving proves the `Subscribe` behind it was READ and
    /// processed while execution frames sat delayed. An order ack would prove
    /// nothing: it is delayed by design.
    ///
    /// Fill condition: the pump holds every event that arrived
    /// within the last `ms`, so saturation needs more than ~1025 events inside
    /// one window (1024 queued plus the one the pump task is sleeping on). Each
    /// submit yields three execution events - OrderAccepted, OrderFilled,
    /// AccountState - so 400 submits is ~1200.
    ///
    /// Admission control may refuse some of the 400 submits rather than hold
    /// them, which is also proof the reader remained alive. This remains a
    /// real listener regression gate: only a silent reader stall fails it.
    #[tokio::test]
    async fn delayed_acks_must_not_stall_the_socket_read_loop() {
        assert!(
            reader_survives_saturation(30_000).await,
            "an armed DelayAcks must delay OUTPUT only: the venue has to keep reading \
             client frames while execution events sit held, so a Subscribe sent behind a \
             saturated held lane must still produce market data. No trade arrived, so \
             output delay stopped input processing. \
             Its control, saturation_witness_control_is_sound, proves the \
             witness itself works with no delay armed"
        );
    }

    /// The control for the measurement above, and NOT ignored - it must stay
    /// green, because without it a failing gate is unreadable.
    ///
    /// Runs the identical saturation with NO delay armed. The pump then drains
    /// as fast as it fills, so it never blocks the reader and the trade must
    /// arrive. That rules out the innocent explanations for a silent witness:
    /// the submits being rejected, the `Subscribe` being malformed, the witness
    /// window being too short, or - the live one, given this tape's documented
    /// arrival droughts - the venue simply having no trade to send.
    #[tokio::test]
    async fn saturation_witness_control_is_sound() {
        assert!(
            reader_survives_saturation(0).await,
            "with no delay armed the held lane cannot block the reader, so the witness \
             trade must arrive; if this fails the measurement gate above proves nothing"
        );
    }

    /// Drives the saturation described on `delayed_acks_must_not_stall_the_socket_read_loop`
    /// at `delay_ms` and reports whether the witness trade arrived.
    async fn reader_survives_saturation(delay_ms: u64) -> bool {
        const SUBMITS: usize = 400;
        const WITNESS_WINDOW: Duration = Duration::from_secs(3);

        let state = state();
        // Arm the delay directly rather than POSTing /control/divergence: the
        // atomic IS the armed state (see `arm_divergence`), and this test is
        // about the pump, not the control plane.
        state
            .accounts
            .acquire(
                &mogwai_protocol::AccountId::parse("TEST-001").unwrap(),
                now_ns(),
            )
            .unwrap()
            .delay_ms
            .store(delay_ms, Ordering::Relaxed);
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");

            // Saturate the pump. Nothing is read back meanwhile; with a delay
            // armed every one of these events is held, so they accumulate.
            for i in 0..SUBMITS {
                let submit = serde_json::to_string(&ClientMessage::SubmitOrder(
                    mogwai_protocol::SubmitOrder {
                        client_order_id: format!("SAT-{i}"),
                        symbol: "BTCUSDT".to_string(),
                        side: mogwai_protocol::Side::Buy,
                        order_type: OrderType::Limit,
                        quantity: rust_decimal::Decimal::new(1, 0),
                        price: Some(rust_decimal::Decimal::new(10_000, 2)),
                        time_in_force: mogwai_protocol::TimeInForce::Gtc,
                    },
                ))
                .expect("encode submit");
                socket
                    .send(tungstenite::Message::Text(submit.into()))
                    .expect("send submit");
            }

            // The question: is the reader still alive behind that backlog?
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], None),
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send subscribe");

            // Bounded wait for the witness. A read timeout on the underlying
            // stream turns "nothing arrived" into an Err rather than a hang, so
            // the stall is a measurement instead of a wedged test.
            if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = socket.get_ref() {
                tcp.set_read_timeout(Some(WITNESS_WINDOW))
                    .expect("set read timeout");
            }
            let deadline = std::time::Instant::now() + WITNESS_WINDOW;
            while std::time::Instant::now() < deadline {
                match socket.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        if let ServerMessage::Trade(_) =
                            serde_json::from_str::<ServerMessage>(&text).expect("decode")
                        {
                            return true;
                        }
                    }
                    Ok(_) => continue,
                    // A close, or a read that timed out with nothing to show:
                    // either way the reader never got to the Subscribe.
                    Err(_) => return false,
                }
            }
            false
        })
        .await
        .expect("blocking ws client")
    }

    #[tokio::test]
    async fn subscribe_issue_names_its_generation_and_symbol() {
        // A `start_ts` below the tape origin used to clamp to the origin in
        // silence - asymmetric with `/trades`, which refuses the identical
        // window with a 422. The subscribe must now announce the shortfall on
        // the wire AND still deliver the origin-anchored stream: the frame is a
        // diagnostic, not a refusal of the live feed. The diagnostic is
        // admission truth, so it rides the PRIORITY lane while the trades ride
        // the data channel; the two race onto the socket - assert both are
        // present rather than pinning a strict interleave the lane split does
        // not guarantee.
        let state = state();
        let data_origin = state.data_origin_ns;
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        let frames = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: vec![mogwai_protocol::SubscriptionRequest {
                    generation: 1,
                    symbol: "BTCUSDT".to_string(),
                    start_ts: Some(data_origin - 3_600_000_000_000),
                    regime: None,
                }],
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send pre-origin subscribe");
            // The diagnostic rides the priority lane and the trades the data
            // channel, so they race; read until both have appeared (bounded so a
            // genuinely missing diagnostic still fails rather than hangs).
            read_until_error_and_trade(&mut socket)
        })
        .await
        .expect("blocking ws client");

        let (outcomes, _) = frames;
        assert!(matches!(
            outcomes.as_slice(),
            [mogwai_protocol::SubscriptionOutcome {
                generation: 1,
                symbol,
                issue: SubscriptionIssue::StartBeforeOrigin {
                    effective_start_ts,
                },
            }] if symbol == "BTCUSDT" && *effective_start_ts == data_origin
        ));
    }

    #[tokio::test]
    async fn subscribe_beyond_sim_now_clamps_to_a_live_stream() {
        // F8: a live Subscribe with a start_ts past sim-now is the WS twin of the
        // /trades future refusal. Rather than honor it (extending the index into
        // the future and emitting unpaced look-ahead ticks), it degrades to a live
        // stream from the clock and announces the clamp on the wire. Under the
        // identity firehose clock the first delivered trade must land at or after
        // sim-now, never in the requested future.
        let state = state(); // identity clock, speed 0 (firehose)
        let sim_now = sim_now_ns(state.sim);
        let future = sim_now + 3_600_000_000_000; // 1h past the clock
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        let frames = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], Some(future)),
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send future subscribe");
            read_until_error_and_trade(&mut socket)
        })
        .await
        .expect("blocking ws client");

        let (outcomes, first_trade_ts) = frames;
        assert!(matches!(
            outcomes.as_slice(),
            [mogwai_protocol::SubscriptionOutcome {
                generation: 1,
                symbol,
                issue: SubscriptionIssue::StartAfterSimNow { .. },
            }] if symbol == "BTCUSDT"
        ));
        assert!(
            first_trade_ts >= sim_now,
            "the clamped stream must start at the live clock: \
             first trade {first_trade_ts} < sim-now {sim_now}"
        );
        assert!(
            first_trade_ts < future,
            "the clamped stream must not deliver the requested future tape: \
             first trade {first_trade_ts} >= requested future {future}"
        );
    }

    /// Read WS frames until both subscription outcomes and a `Trade` arrive,
    /// returning the outcomes and first trade's `ts_event`. Bounded
    /// so a genuinely absent diagnostic (or trade) fails the test rather than
    /// hanging. Shared by the below-origin and future-clamp subscribe tests, whose
    /// diagnostic (priority lane) and trades (data channel) race onto the socket.
    fn read_until_error_and_trade<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> (Vec<mogwai_protocol::SubscriptionOutcome>, u64) {
        let mut outcomes = None;
        let mut first_trade_ts: Option<u64> = None;
        for _ in 0..2_000 {
            if outcomes.is_some() && first_trade_ts.is_some() {
                break;
            }
            match socket.read().expect("read ws frame") {
                tungstenite::Message::Text(text) => {
                    let message =
                        serde_json::from_str::<ServerMessage>(&text).expect("decode ServerMessage");
                    if let ServerMessage::SubscriptionIssues { entries, .. } = message {
                        outcomes = Some(entries);
                    } else if let ServerMessage::Trade(t) = message {
                        first_trade_ts.get_or_insert(t.ts_event);
                    }
                }
                tungstenite::Message::Close(_) => panic!("socket closed prematurely"),
                _ => continue,
            }
        }
        (
            outcomes.expect("a subscription issue must arrive"),
            first_trade_ts.expect("a trade must arrive"),
        )
    }

    /// One `Subscribe` entry per symbol, generations numbered from 1 and one
    /// shared `start_ts`. The shape almost every test wants; tests that turn on
    /// a specific generation or a per-entry cursor build their entries by hand.
    fn subscribe_entries<S: AsRef<str>>(
        symbols: impl IntoIterator<Item = S>,
        start_ts: Option<u64>,
    ) -> Vec<mogwai_protocol::SubscriptionRequest> {
        symbols
            .into_iter()
            .enumerate()
            .map(|(i, symbol)| mogwai_protocol::SubscriptionRequest {
                generation: i as u64 + 1,
                symbol: symbol.as_ref().to_string(),
                start_ts,
                regime: None,
            })
            .collect()
    }

    /// Read frames until the first `Trade`, returning its `ts_event`. Bounded so
    /// a silent feed fails the caller rather than hanging.
    fn read_until_trade<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> u64 {
        for _ in 0..2_000 {
            match socket.read().expect("read ws frame") {
                tungstenite::Message::Text(text) => {
                    if let ServerMessage::Trade(t) =
                        serde_json::from_str::<ServerMessage>(&text).expect("decode ServerMessage")
                    {
                        return t.ts_event;
                    }
                }
                tungstenite::Message::Close(_) => panic!("socket closed before any trade"),
                _ => {}
            }
        }
        panic!("no trade arrived within the frame budget");
    }

    /// Read frames until the first typed subscription outcome. Bounded so an
    /// absent diagnostic fails the caller rather than hanging.
    fn read_until_subscription_issues<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> (Vec<mogwai_protocol::SubscriptionOutcome>, usize, usize) {
        for _ in 0..2_000 {
            match socket.read().expect("read ws frame") {
                tungstenite::Message::Text(text) => {
                    if let ServerMessage::SubscriptionIssues {
                        entries,
                        issues_total,
                        refusals_total,
                        ..
                    } =
                        serde_json::from_str::<ServerMessage>(&text).expect("decode ServerMessage")
                    {
                        return (entries, issues_total, refusals_total);
                    }
                }
                tungstenite::Message::Close(_) => panic!("socket closed before any error"),
                _ => {}
            }
        }
        panic!("no subscription issue arrived within the frame budget");
    }

    #[tokio::test]
    async fn replay_cap_refuses_subscribe_across_connections() {
        // S22a: every subscribed symbol runs on its own OS thread, so the global
        // `max_concurrent_replays` pool is what keeps a fleet of connections from
        // spawning replay threads without bound. With a cap of one, the first
        // connection's live BTCUSDT stream holds the only permit; a SECOND
        // connection subscribing the same symbol cannot take one and is refused
        // with a `ProtocolError` on the wire while the first stream keeps running
        // untouched. The permit pool is an `Arc<Semaphore>` in the shared state,
        // so the cap spans connections - which is the whole point, a per-
        // connection cap would not bound the aggregate thread count.
        let state = state();
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        tokio::task::spawn_blocking(move || {
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], None),
            })
            .expect("encode subscribe");

            // Connection one takes the only permit and must reach a live trade
            // before connection two races in - that proves the permit is HELD,
            // not merely requested. Its socket stays in scope for the rest of the
            // closure so the replay thread, and thus the permit, lives on.
            let (mut hold, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect one");
            hold.send(tungstenite::Message::Text(subscribe.clone().into()))
                .expect("send subscribe one");
            read_until_trade(&mut hold);

            // A second clean subscriber attaches to the existing tape without
            // consuming another tape permit.
            let (mut denied, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-002"))
                .expect("ws connect two");
            denied
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send subscribe two");
            read_until_trade(&mut denied);
        })
        .await
        .expect("blocking ws client");
    }

    /// S22a under admission control: a subscribe naming the whole cap in
    /// unknown symbols must cost ONE priority frame and leave the connection up.
    ///
    /// Per-symbol refusals coalesce for exactly this reason. Emitted one frame
    /// per symbol they would overrun the 64-slot priority lane on the 65th
    /// symbol and close a connection that `config.rs`'s S22a contract promises
    /// stays open - a capacity bound turning into a functional refusal.
    #[tokio::test]
    async fn coalesced_subscribe_refusal_keeps_the_connection_up() {
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        let frames = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            let unknown = (0..mogwai_protocol::MAX_SUBSCRIBE_SYMBOLS).map(|i| format!("NOPE{i}"));
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(unknown, None),
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send all-unknown subscribe");
            let refusal = match socket.read().expect("read ws frame") {
                tungstenite::Message::Text(text) => {
                    serde_json::from_str::<ServerMessage>(&text).expect("decode ServerMessage")
                }
                other => panic!("expected the coalesced refusal, got {other:?}"),
            };
            // The connection is still usable: a good subscribe sent behind the
            // refusal still streams.
            let good = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], None),
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(good.into()))
                .expect("the connection is still up");
            let trade_ts = read_until_trade(&mut socket);
            (refusal, trade_ts)
        })
        .await
        .expect("blocking ws client");

        let (refusal, trade_ts) = frames;
        let ServerMessage::SubscriptionIssues {
            entries,
            issues_total,
            refusals_total,
            ..
        } = refusal
        else {
            panic!("expected ONE coalesced Subscribe refusal, got {refusal:?}")
        };
        assert_eq!(
            refusals_total,
            mogwai_protocol::MAX_SUBSCRIBE_SYMBOLS,
            "the true count is reported even though the list is truncated"
        );
        assert_eq!(
            entries.len(),
            mogwai_protocol::MAX_SUBSCRIPTION_ISSUES_LISTED,
            "the listed symbols are capped, which is what bounds the frame"
        );
        assert!(trade_ts > 0, "the surviving connection still streams");
        assert_eq!(
            issues_total,
            mogwai_protocol::MAX_SUBSCRIBE_SYMBOLS,
            "issues_total counts every outcome, truncated or not"
        );
    }

    /// The assertion the 16-entry cap is paid for with. A subscribe whose
    /// degradations outnumber the cap must still list every refusal, because a
    /// degradation lost to truncation costs a log line and a refusal lost to it
    /// costs a feed. Driven against the pure coalescer: the venue lists one
    /// symbol, so 20 degradations cannot be produced over a real socket.
    #[test]
    fn subscribe_issues_list_refusals_before_degradations() {
        let degradations: Vec<mogwai_protocol::SubscriptionOutcome> = (0..20)
            .map(|i| mogwai_protocol::SubscriptionOutcome {
                generation: i + 1,
                symbol: format!("DEGRADED{i}"),
                issue: SubscriptionIssue::StartBeforeOrigin {
                    effective_start_ts: 42,
                },
            })
            .collect();
        let refusals: Vec<mogwai_protocol::SubscriptionOutcome> = (0..3)
            .map(|i| mogwai_protocol::SubscriptionOutcome {
                generation: 100 + i,
                symbol: format!("NOPE{i}"),
                issue: SubscriptionIssue::UnknownSymbol,
            })
            .collect();

        let entries = coalesce_issues(refusals, degradations);

        assert_eq!(
            entries.len(),
            mogwai_protocol::MAX_SUBSCRIPTION_ISSUES_LISTED
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.issue.is_refusal())
                .count(),
            3,
            "all three refusals survive the cap despite being discovered last"
        );
        assert!(
            entries[..3].iter().all(|entry| entry.issue.is_refusal()),
            "refusals lead the list"
        );
    }

    /// A generation that does not advance is a client-ordering fault, and the
    /// answer is to refuse the ENTRY, never to destroy the healthy replay the
    /// current generation is feeding. The witness is a trade after the refusal.
    #[tokio::test]
    async fn subscribe_issue_refuses_a_stale_generation_without_killing_the_stream() {
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        let (entries, trade_ts) = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            socket
                .send(tungstenite::Message::Text(
                    subscribe_at_generation("BTCUSDT", 5).into(),
                ))
                .expect("send generation 5");
            read_until_trade(&mut socket);
            socket
                .send(tungstenite::Message::Text(
                    subscribe_at_generation("BTCUSDT", 3).into(),
                ))
                .expect("send generation 3");
            let (entries, _, _) = read_until_subscription_issues(&mut socket);
            // The generation-5 replay is untouched and still delivering.
            (entries, read_until_trade(&mut socket))
        })
        .await
        .expect("blocking ws client");

        assert!(matches!(
            entries.as_slice(),
            [mogwai_protocol::SubscriptionOutcome {
                generation: 3,
                symbol,
                issue: SubscriptionIssue::StaleGeneration { current: 5 },
            }] if symbol == "BTCUSDT"
        ));
        assert!(trade_ts > 0, "the generation-5 stream keeps delivering");
    }

    /// The high-water map of the monotonicity check is connection-lifetime and
    /// independent of `replays`. Against `replays` alone the post-unsubscribe
    /// entry is accepted and the whole generation-reuse hazard is back: a
    /// delayed diagnostic about the old stream would look current.
    #[tokio::test]
    async fn subscribe_issue_remembers_a_generation_across_unsubscribe() {
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        let entries = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            socket
                .send(tungstenite::Message::Text(
                    subscribe_at_generation("BTCUSDT", 5).into(),
                ))
                .expect("send generation 5");
            read_until_trade(&mut socket);
            let unsubscribe = serde_json::to_string(&ClientMessage::Unsubscribe {
                symbols: vec!["BTCUSDT".to_string()],
            })
            .expect("encode unsubscribe");
            socket
                .send(tungstenite::Message::Text(unsubscribe.into()))
                .expect("send unsubscribe");
            socket
                .send(tungstenite::Message::Text(
                    subscribe_at_generation("BTCUSDT", 4).into(),
                ))
                .expect("send generation 4");
            let (entries, _, _) = read_until_subscription_issues(&mut socket);
            entries
        })
        .await
        .expect("blocking ws client");

        assert!(matches!(
            entries.as_slice(),
            [mogwai_protocol::SubscriptionOutcome {
                generation: 4,
                issue: SubscriptionIssue::StaleGeneration { current: 5 },
                ..
            }]
        ));
    }

    /// Two entries naming one symbol have no defined meaning - which cursor
    /// wins, which generation is current - so the whole FRAME is refused at the
    /// boundary and no replay is spawned for either. The old `dedup_symbols`
    /// answer silently discarded one of the two cursors.
    #[tokio::test]
    async fn subscribe_issue_refuses_a_duplicated_symbol_frame_wide() {
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        let messages = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            let duplicated = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: vec![
                    mogwai_protocol::SubscriptionRequest {
                        generation: 1,
                        symbol: "BTCUSDT".to_string(),
                        start_ts: None,
                        regime: None,
                    },
                    mogwai_protocol::SubscriptionRequest {
                        generation: 2,
                        symbol: "BTCUSDT".to_string(),
                        start_ts: None,
                        regime: None,
                    },
                ],
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(duplicated.into()))
                .expect("send duplicated subscribe");
            let refusal = read_until_text(&mut socket);
            // A generation-1 subscribe afterwards must be ACCEPTED. If either
            // duplicated entry had been processed, the generation high-water map
            // would already hold 1 or 2 for BTCUSDT and this would come back as
            // StaleGeneration instead of streaming - so a trade here is the
            // witness that the frame was refused before any bookkeeping ran.
            socket
                .send(tungstenite::Message::Text(
                    subscribe_at_generation("BTCUSDT", 1).into(),
                ))
                .expect("send a clean subscribe");
            (refusal, read_until_trade(&mut socket))
        })
        .await
        .expect("blocking ws client");

        let (refusal, trade_ts) = messages;
        let decoded =
            serde_json::from_str::<ServerMessage>(&refusal).expect("decode ServerMessage");
        assert!(
            matches!(decoded, ServerMessage::ProtocolError { .. }),
            "a duplicated symbol is a whole-frame fault, not a per-entry one: {decoded:?}"
        );
        assert!(
            trade_ts > 0,
            "no replay was spawned for either duplicated entry"
        );
    }

    /// An out-of-range regime is a DEGRADATION, not a refusal: it is reported
    /// per entry AND the clean, unhavocked tape still streams. It used to be
    /// dropped in silence, telling the client nothing.
    #[tokio::test]
    async fn subscribe_issue_reports_an_out_of_range_regime_and_still_streams() {
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });

        let (outcomes, trade_ts) = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: vec![mogwai_protocol::SubscriptionRequest {
                    generation: 9,
                    symbol: "BTCUSDT".to_string(),
                    start_ts: None,
                    regime: Some(MarketRegime::LiquidityDrought { thin_factor: 0.5 }),
                }],
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send subscribe");
            read_until_error_and_trade(&mut socket)
        })
        .await
        .expect("blocking ws client");

        assert!(matches!(
            outcomes.as_slice(),
            [mogwai_protocol::SubscriptionOutcome {
                generation: 9,
                symbol,
                issue: SubscriptionIssue::InvalidRegime,
            }] if symbol == "BTCUSDT"
        ));
        assert!(trade_ts > 0, "the clean tape streams despite the drop");
    }

    /// One symbol at one generation, encoded for the socket.
    fn subscribe_at_generation(symbol: &str, generation: u64) -> String {
        serde_json::to_string(&ClientMessage::Subscribe {
            subscriptions: vec![mogwai_protocol::SubscriptionRequest {
                generation,
                symbol: symbol.to_string(),
                start_ts: None,
                regime: None,
            }],
        })
        .expect("encode subscribe")
    }

    /// A resting limit buy, priced far under the market so it never fills and
    /// the book's shape (balances, positions) stays put between submits.
    fn resting_submit(id: &str) -> String {
        serde_json::to_string(&ClientMessage::SubmitOrder(SubmitOrder {
            client_order_id: id.to_string(),
            symbol: "BTCUSDT".to_string(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Decimal::new(1, 0),
            price: Some(Decimal::new(10_000, 2)),
            time_in_force: TimeInForce::Gtc,
        }))
        .expect("encode submit")
    }

    /// Serve `state` on an ephemeral port and return its address. The four
    /// admission tests below all need the same three lines.
    async fn serve_ws(state: AppState) -> std::net::SocketAddr {
        let app = Router::new()
            .route("/ws", get(ws_upgrade))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            drop(axum::serve(listener, app).await);
        });
        addr
    }

    /// I2 over a real socket: a command whose worst-case output cannot be
    /// reserved never reaches the engine.
    ///
    /// The held budget is set below one submit's worst case, so the reservation
    /// in `process_order_cmd` fails BEFORE the engine lock's `process` call. The
    /// refusal is visible (I5) and, crucially, the venue's own truth channel
    /// agrees: a `QueryOrders` afterwards reports an empty book - no venue order
    /// id burned, no order resting. A refusal that had let the engine mutate
    /// first would show the order here and leave the client and the venue
    /// permanently disagreeing about whether it exists.
    ///
    /// The budget is a config knob for exactly this reason: at the shipped 8 MiB
    /// this branch is reachable only by first pushing megabytes of held output
    /// through a stalled connection, and the resulting test would measure timing
    /// luck rather than the invariant. `QueryOrders` still fits the small budget
    /// (an empty book's snapshot is a few hundred bytes), so the witness is not
    /// itself refused.
    #[tokio::test]
    async fn reservation_failure_leaves_engine_state_untouched() {
        let mut state = state();
        state.cfg.exec_held_budget_bytes = 4096;
        let addr = serve_ws(state).await;

        let (refusal, snapshot) = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            socket
                .send(tungstenite::Message::Text(
                    resting_submit("UNRESERVABLE").into(),
                ))
                .expect("send submit");
            let refusal = read_until_admission_rejected(&mut socket);
            let query = serde_json::to_string(&ClientMessage::QueryOrders {
                request_id: "Q-AFTER-REFUSAL".to_string(),
                client_order_id: None,
                open_only: false,
            })
            .expect("encode query");
            socket
                .send(tungstenite::Message::Text(query.into()))
                .expect("send query");
            let snapshot = read_until_order_snapshot(&mut socket);
            (refusal, snapshot)
        })
        .await
        .expect("blocking ws client");

        let ServerMessage::AdmissionRejected { subject, .. } = &refusal else {
            unreachable!("the reader returns only AdmissionRejected")
        };
        assert!(
            matches!(
                subject,
                mogwai_protocol::AdmissionSubject::Submit { client_order_id }
                    if client_order_id == "UNRESERVABLE"
            ),
            "the refusal is attributed to the submit it refused: {subject:?}"
        );
        assert!(
            snapshot.orders.is_empty(),
            "a refused submit must not have reached the engine, yet the venue \
             reports {:?}",
            snapshot.orders
        );
    }

    /// I6 over a real socket: when the priority lane cannot take a refusal, the
    /// connection ends with a STATED reason rather than a silent stall.
    ///
    /// Reaching the condition needs the priority lane to actually back up, and a
    /// queued frame only holds its slot until the writer writes it - so the
    /// writer has to be parked. The peer subscribes the firehose and then stops
    /// reading, which fills its receive window and parks the writer inside
    /// `sink.send`; the refusals it then provokes with undecodable frames pile up
    /// unwritten, and the lane (two slots here, 64 in production) runs out. The
    /// peer resumes reading only to collect what the venue said on the way out.
    ///
    /// `admission_lane_frames` is a config knob because at 64 this setup would
    /// need 64 refusals to survive a race against a writer that may drain at any
    /// moment; at two it is deterministic and tests the identical branch.
    #[tokio::test]
    async fn admission_lane_overload_closes_with_a_reason() {
        let mut state = state();
        state.cfg.admission_lane_frames = 2;
        let addr = serve_ws(state).await;

        let close = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            park_the_writer(&mut socket);
            // Undecodable frames: each is admission truth the venue must state,
            // and each takes a priority slot it cannot get back while the writer
            // is parked. Exactly one more than the lane holds, and not one
            // further: the read loop stops at the overload, so any extra frames
            // sit unread in the venue's receive queue and make the kernel answer
            // the socket's drop with an RST - which discards this client's
            // receive buffer, close frame and all. The reasoned close would then
            // be lost to the transport rather than to the venue.
            for _ in 0..3 {
                socket
                    .send(tungstenite::Message::Text("{\"type\":\"Nonsense\"}".into()))
                    .expect("send undecodable frame");
            }
            read_until_close(&mut socket)
        })
        .await
        .expect("blocking ws client");

        let close = close.expect("an overload close, not a silent stall or a drop");
        assert_eq!(
            u16::from(close.code),
            CLOSE_ADMISSION_OVERLOAD,
            "the close names the overload with WS 1013 Try Again Later"
        );
        assert!(
            !close.reason.is_empty(),
            "a close without a reason is the silence this invariant forbids"
        );
    }

    /// The other half of 5.8: the reasoned close is best-effort, releasing the
    /// connection's resources is not.
    ///
    /// The peer here never reads at all, so the writer stays parked in
    /// `sink.send` and the close frame is never written. Teardown must not block
    /// behind it: after `CLOSE_GRACE` the writer is aborted and the socket
    /// dropped. Two things a client can see prove it happened:
    ///
    /// - the replay pool, capped at one, hands its permit to a SECOND connection
    ///   - so the wedged connection's replay threads were cancelled and joined;
    /// - the wedged socket itself is dead, and dead WITHOUT a reasoned close on
    ///   it. That absence is the load-bearing half. Had teardown awaited the
    ///   writer unconditionally, the writer would still be parked when this peer
    ///   finally reads, would then unpark, and would deliver its whole backlog
    ///   AND the close - so "ended, with no close" distinguishes the forced
    ///   teardown from the patient one, which is exactly 5.8's claim: the
    ///   reasoned close is attempted, the teardown does not depend on it.
    #[tokio::test]
    async fn overload_close_terminates_against_a_nonreading_peer() {
        let mut state = state();
        state.cfg.admission_lane_frames = 1;
        let addr = serve_ws(state).await;

        let (trade_ts, wedged_end) = tokio::task::spawn_blocking(move || {
            let (mut wedged, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-002"))
                .expect("ws connect one");
            park_the_writer(&mut wedged);
            for _ in 0..2 {
                wedged
                    .send(tungstenite::Message::Text("{\"type\":\"Nonsense\"}".into()))
                    .expect("send undecodable frame");
            }
            // Nothing is read from `wedged` while the venue gives up on it -
            // exactly the peer 5.8 describes.
            std::thread::sleep(CLOSE_GRACE + Duration::from_secs(2));

            let (mut fresh, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect two");
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], None),
            })
            .expect("encode subscribe");
            fresh
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send subscribe");
            let ts = read_until_trade(&mut fresh);

            // A read timeout turns "the venue is still holding this socket open"
            // into a failure rather than a hang.
            if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = wedged.get_ref() {
                tcp.set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set read timeout");
            }
            let end = drain_until_end(&mut wedged);
            (ts, end)
        })
        .await
        .expect("blocking ws client");

        assert!(
            trade_ts > 0,
            "the only replay permit was still held, so the overloaded connection \
             never finished tearing down"
        );
        assert_eq!(
            wedged_end,
            PeerEnd::Dropped,
            "the venue must have given up on this peer within CLOSE_GRACE and \
             dropped the socket: StillOpen means teardown is waiting on it, and \
             Close means the writer was never parked so the setup proved nothing"
        );
    }

    /// The held budget is a ceiling on produced-but-UNWRITTEN bytes, so writing
    /// a frame must give its bytes back - end to end, through the real writer.
    ///
    /// Sized at exactly one submit's worst case, five sequential submits are
    /// possible only if each batch's charge returns as the writer puts it on the
    /// socket. A leak of a single byte refuses the second one. The same budget
    /// then proves the other direction: with a 30 s `DelayAcks` armed nothing is
    /// written, the budget stays charged, and the next submit is refused - the
    /// refusal itself arriving promptly, because admission is not held (I4).
    /// Clearing the delay drains the held frames and the budget comes back, so
    /// submits are admitted again.
    #[tokio::test]
    async fn held_budget_is_returned_on_write_and_on_disconnect() {
        let state = state();
        let slot = state
            .accounts
            .acquire(
                &mogwai_protocol::AccountId::parse("TEST-001").unwrap(),
                now_ns(),
            )
            .unwrap();
        let delay_ms = Arc::clone(&slot);
        let mut state = state;
        // One submit's worst case, against the widest book these submits can
        // produce: accepting an order can introduce the balance and position
        // rows the empty book has not got yet, and `worst_case_output_bytes`
        // widens by exactly that much itself, so sizing against the pre-command
        // shape would leave the budget a few hundred bytes short of the SECOND
        // command and the test would fail for arithmetic rather than for a leak.
        // It is still one command's budget: two of these never fit.
        let shape = slot.engine.lock().await.book_shape();
        state.cfg.exec_held_budget_bytes = mogwai_protocol::sizing::worst_case_output_bytes(
            &serde_json::from_str::<ClientMessage>(&resting_submit("SIZE")).expect("decode"),
            &mogwai_protocol::sizing::BookShape {
                balances: shape.balances + 2,
                positions: shape.positions + 1,
                ..shape
            },
        );
        let budget = state.cfg.exec_held_budget_bytes;
        let addr = serve_ws(state).await;

        tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-002"))
                .expect("ws connect");
            for i in 0..5 {
                socket
                    .send(tungstenite::Message::Text(
                        resting_submit(&format!("DRAIN-{i}")).into(),
                    ))
                    .expect("send submit");
                // Read the whole batch out (the AccountState is its last frame),
                // which is what makes the budget available for the next one.
                read_until_account_state(&mut socket, &format!("DRAIN-{i}"));
            }

            // Nothing is written while the delay is armed, so the budget stays
            // charged and the SECOND submit cannot be reserved. The window is
            // seconds rather than the spec's illustrative 30 s because a frame
            // already dequeued by the pump is sleeping out ITS deadline -
            // clearing the divergence reaches frames still queued, not the one
            // in the pump's hand - so a 30 s arm would make the drain below a
            // 30 s wait rather than a test.
            delay_ms.delay_ms.store(2_000, Ordering::Relaxed);
            socket
                .send(tungstenite::Message::Text(resting_submit("HELD").into()))
                .expect("send held submit");
            socket
                .send(tungstenite::Message::Text(resting_submit("REFUSED").into()))
                .expect("send unreservable submit");
            let refusal = read_until_admission_rejected(&mut socket);
            let ServerMessage::AdmissionRejected { subject, .. } = &refusal else {
                unreachable!("the reader returns only AdmissionRejected")
            };
            assert!(
                matches!(
                    subject,
                    mogwai_protocol::AdmissionSubject::Submit { client_order_id }
                        if client_order_id == "REFUSED"
                ),
                "the refusal arrives while the held batch is still held: {subject:?}"
            );

            // The held batch reaches the writer when its own deadline expires,
            // and the writer returns its bytes as it writes them; the cleared
            // delay keeps the final submit prompt.
            delay_ms.delay_ms.store(0, Ordering::Relaxed);
            read_until_account_state(&mut socket, "HELD");
            socket
                .send(tungstenite::Message::Text(
                    resting_submit("AFTER-DRAIN").into(),
                ))
                .expect("send post-drain submit");
            read_until_account_state(&mut socket, "AFTER-DRAIN");
        })
        .await
        .expect("blocking ws client");

        assert!(
            budget > 0,
            "the test is only meaningful against a real one-command budget"
        );
    }

    /// 3.3a's not-optional clause: a resubscribe reserves the promise of its
    /// replay's ONE possible diagnostic (the dead-seek) BEFORE it quiesces the
    /// stream that replay replaces.
    ///
    /// The promise pool is sized at one here, so the live BTCUSDT stream holds
    /// the only ticket and the resubscribe of that same symbol cannot get one.
    /// Reserving after the quiesce would destroy a healthy stream and only then
    /// discover it has nothing to say about why; reserving first means the
    /// connection ends with the overload reason STATED while the in-flight
    /// replay is still running.
    ///
    /// The ORDER is what this measures, and the close is what measures it: a
    /// quiesce joins the old replay thread, which drops that replay's promise
    /// ticket - so a handler that quiesced first would find the pool free again
    /// and sail through, tearing down a live stream on a connection that never
    /// learned it was at its limit. The close therefore only exists in the
    /// correct ordering. The trade assertions carry the rest of the clause: the
    /// original stream was live and stayed ascending, i.e. it was not replaced
    /// by a re-seeking one on the way out.
    #[tokio::test]
    async fn subscribe_reserves_diagnostic_capacity_before_quiescing() {
        let mut state = state();
        state.cfg.admission_promise_tickets = 1;
        let addr = serve_ws(state).await;

        let (trades_before, trades_after, close) = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], None),
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(subscribe.clone().into()))
                .expect("send subscribe");
            // The stream is live, so its promise ticket is genuinely held.
            let first_ts = read_until_trade(&mut socket);

            let resubscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: vec![mogwai_protocol::SubscriptionRequest {
                    generation: 2,
                    symbol: "BTCUSDT".to_string(),
                    start_ts: None,
                    regime: None,
                }],
            })
            .expect("encode resubscribe");
            socket
                .send(tungstenite::Message::Text(resubscribe.into()))
                .expect("send resubscribe");
            let mut after: Vec<u64> = Vec::new();
            let mut close = None;
            for _ in 0..200_000 {
                match socket.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        if let ServerMessage::Trade(t) =
                            serde_json::from_str::<ServerMessage>(&text)
                                .expect("decode ServerMessage")
                        {
                            after.push(t.ts_event);
                        }
                    }
                    Ok(tungstenite::Message::Close(frame)) => {
                        close = frame;
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            (first_ts, after, close)
        })
        .await
        .expect("blocking ws client");

        let close = close.expect("the refused resubscribe must state its reason, not go silent");
        assert_eq!(
            u16::from(close.code),
            CLOSE_ADMISSION_OVERLOAD,
            "the close names the admission overload"
        );
        assert!(
            close.reason.contains("diagnostic capacity"),
            "the close names the promise pool it could not draw from: {}",
            close.reason
        );
        assert!(trades_before > 0, "the original stream was live");
        assert!(
            trades_after.iter().all(|ts| *ts > trades_before),
            "the in-flight replay was neither torn down nor replaced by a \
             re-seeking one: its trades stay ahead of the last pre-resubscribe \
             tick, got {trades_after:?} after {trades_before}"
        );
        assert!(
            trades_after.windows(2).all(|w| w[1] > w[0]),
            "a stream quiesced and replaced before the refusal would seam here: \
             {trades_after:?}"
        );
    }

    /// 5.7: with one lane's sender dropped and the other still alive, the writer
    /// PARKS. The per-branch disable flags are what create that property - a
    /// bare `biased` select over both receivers sees the closed one answer
    /// `None` instantly and forever, and never reaches an await that yields.
    ///
    /// The failure is not a hang - tokio's cooperative budget makes even a
    /// tight `recv()` loop yield every so often, so a starvation probe would
    /// pass against the naive shape. It is a BURNED CORE, so this measures CPU:
    /// the writer gets a dedicated thread with its own current-thread runtime,
    /// and that thread's own utime+stime (from `/proc/thread-self/stat`, so no
    /// other test's load is in the reading) is sampled across a wall window in
    /// which the venue has nothing whatsoever to write. A parked writer spends
    /// approximately none of it; the flagless `select!` spends all of it.
    #[test]
    fn writer_does_not_spin_when_one_lane_closes() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("current-thread runtime");
            rt.block_on(async move {
                let (held_tx, held_rx) = mpsc::channel::<Outbound>(8);
                let (prio_tx, prio_rx) = mpsc::unbounded_channel::<Outbound>();
                // Same thread as the measurement: `spawn` on a current-thread
                // runtime, so the writer's CPU is this thread's CPU.
                let writer = tokio::spawn(run_writer(
                    futures_util::sink::drain(),
                    prio_rx,
                    held_rx,
                    SimClock::identity(),
                    crate::accounts::AccountSlot::detached_for_tests(),
                ));
                // One lane closes; the priority lane stays open, which is the
                // exact asymmetry the naive select! spins on.
                drop(held_tx);
                let before = thread_cpu_ticks();
                tokio::time::sleep(Duration::from_millis(400)).await;
                let burned = thread_cpu_ticks() - before;
                // The parked writer is still a writer: the surviving lane still
                // reaches it, and its close still ends the task.
                prio_tx
                    .send(Outbound::Close(crate::admission::CloseSpec::overload(
                        "end of test",
                    )))
                    .expect("the priority lane is still open");
                writer.await.expect("the writer ends on the close");
                done_tx.send(burned).ok();
            });
        });

        let burned = done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the writer thread never reported");
        // Ticks, not seconds, so no assumption about `_SC_CLK_TCK` beyond it
        // being coarse: 400 ms of wall with nothing to write is tens of ticks
        // when the loop spins and zero or one when it parks.
        assert!(
            burned <= 4,
            "the writer burned {burned} CPU ticks with both lanes idle and one \
             closed: it is spinning on the closed receiver rather than parking"
        );
    }

    /// This thread's own CPU time (utime + stime) in kernel ticks. Per-thread
    /// rather than per-process because the test binary's other threads would
    /// otherwise drown the signal.
    fn thread_cpu_ticks() -> u64 {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").expect("read thread stat");
        // The comm field is parenthesized and may itself contain spaces, so
        // fields are counted from after its closing paren: utime and stime are
        // the 12th and 13th from there.
        let rest = stat
            .rsplit_once(')')
            .expect("stat carries a parenthesized comm")
            .1;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let utime: u64 = fields[11].parse().expect("utime");
        let stime: u64 = fields[12].parse().expect("stime");
        utime + stime
    }

    /// 5.3's ceiling is only provable if EVERY admission frame's reason is
    /// truncated, and the two `ProtocolError` sites are the ones that carry
    /// client-controlled text: serde's decode-error string quotes the unknown
    /// variant it was handed, and the unservable-subscribe diagnostic
    /// interpolates the client's symbol. Both come back under
    /// `ADMISSION_FRAME_MAX_BYTES`, which is what makes the priority lane's
    /// FRAME count a memory bound (3.3). Measured on the bytes the client
    /// actually received, over a real socket, so it gates the production sites
    /// rather than `truncate_reason` itself.
    #[tokio::test]
    async fn protocol_error_reasons_are_truncated() {
        let addr = serve_ws(state()).await;

        let (undecodable, unknown_symbol) = tokio::task::spawn_blocking(move || {
            let (mut socket, _) = tungstenite::connect(format!("ws://{addr}/ws?account=TEST-001"))
                .expect("ws connect");
            // A megabyte of client-controlled text inside serde's own error
            // message: the variant name is echoed back verbatim by
            // `serde_json::Error::to_string`.
            let nonsense = format!("{{\"type\":\"{}\"}}", "N".repeat(1024 * 1024));
            socket
                .send(tungstenite::Message::Text(nonsense.into()))
                .expect("send undecodable frame");
            let undecodable = read_until_text(&mut socket);

            // A symbol at exactly the wire cap: long enough to pass validation,
            // unknown enough to be refused, and interpolated into the reason.
            let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
                subscriptions: subscribe_entries(
                    [&*"Z".repeat(mogwai_protocol::MAX_SYMBOL_LEN)],
                    None,
                ),
            })
            .expect("encode subscribe");
            socket
                .send(tungstenite::Message::Text(subscribe.into()))
                .expect("send unknown-symbol subscribe");
            let unknown_symbol = read_until_text(&mut socket);
            (undecodable, unknown_symbol)
        })
        .await
        .expect("blocking ws client");

        for frame in [&undecodable, &unknown_symbol] {
            let msg = serde_json::from_str::<ServerMessage>(frame).expect("decode ServerMessage");
            assert!(
                matches!(
                    msg,
                    ServerMessage::ProtocolError { .. }
                        | ServerMessage::AdmissionRejected { .. }
                        | ServerMessage::SubscriptionIssues { .. }
                ),
                "expected admission truth, got {msg:?}"
            );
            assert!(
                frame.len() <= mogwai_protocol::ADMISSION_FRAME_MAX_BYTES,
                "an admission frame of {} bytes breaks the lane's frame-count \
                 bound: {frame}",
                frame.len()
            );
        }
        assert!(
            !undecodable.contains(&"N".repeat(1024)),
            "the undecodable-frame diagnostic echoed the client's text back \
             unbounded"
        );
    }

    /// Read the next text frame, returning it raw: the SERIALIZED bytes are what
    /// a per-frame ceiling is a claim about.
    fn read_until_text<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> String {
        for _ in 0..2_000 {
            match socket.read().expect("read ws frame") {
                tungstenite::Message::Text(text) => return text.to_string(),
                tungstenite::Message::Close(_) => panic!("socket closed with no reply"),
                _ => {}
            }
        }
        panic!("no frame arrived within the frame budget");
    }

    /// 5.1's control-plane clause: `RejectNextSubmit.reason` is operator-supplied
    /// and the engine echoes it VERBATIM into `OrderRejected.reason`, so it is
    /// truncated at the arming boundary or the size model is not an upper bound
    /// at all - a 1 MiB armed reason would make a produced frame exceed the
    /// reservation `worst_case_output_bytes` granted for it.
    ///
    /// Both halves are asserted on the frame the venue actually produced: its
    /// reason stops at `MAX_REASON_LEN` (only the boundary can have done that -
    /// nothing downstream touches the stored string) and the whole frame fits
    /// `ORDER_EVENT_MAX_BYTES`.
    #[tokio::test]
    async fn armed_reject_reason_is_truncated_at_the_control_boundary() {
        let state = state();
        let response = arm_divergence(
            State(state.clone()),
            account_headers(),
            Json(mogwai_protocol::control::Divergence::RejectNextSubmit {
                reason: "R".repeat(1024 * 1024),
            }),
        )
        .await;
        let response = axum::response::IntoResponse::into_response(response);
        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "the arm is accepted"
        );

        let events = submit_order_http(
            State(state),
            account_headers(),
            Json(ClientMessage::SubmitOrder(SubmitOrder {
                client_order_id: "ARMED-REJECT".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: OrderType::Limit,
                quantity: "1".parse().expect("decimal"),
                price: Some("100".parse().expect("decimal")),
                time_in_force: TimeInForce::Gtc,
            })),
        )
        .await
        .expect("route answers");

        let ServerMessage::OrderRejected { reason, .. } = &events.0[0] else {
            panic!("expected the armed rejection, got {:?}", events.0[0]);
        };
        assert_eq!(
            reason.len(),
            mogwai_protocol::MAX_REASON_LEN,
            "the operator's megabyte was truncated where it was armed, not \
             echoed at full length"
        );
        let frame = serde_json::to_string(&events.0[0]).expect("serialize the rejection");
        assert!(
            frame.len() <= mogwai_protocol::sizing::ORDER_EVENT_MAX_BYTES,
            "an armed reason must not push a produced frame past the bytes \
             reserved for it: {} bytes",
            frame.len()
        );
    }

    /// Subscribe the firehose and stop reading until the venue's writer is
    /// parked in `sink.send` against a full receive window. Everything the venue
    /// produces afterwards stays queued, holding its budget - which is what puts
    /// a lane into overload without a load generator.
    fn park_the_writer<S: std::io::Read + std::io::Write>(socket: &mut tungstenite::WebSocket<S>) {
        let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
            subscriptions: subscribe_entries(["BTCUSDT"], None),
        })
        .expect("encode subscribe");
        socket
            .send(tungstenite::Message::Text(subscribe.into()))
            .expect("send subscribe");
        // The unthrottled generator fills the socket in well under this; the
        // wait is wall time because a TCP window is a transport fact, not a
        // simulated one.
        std::thread::sleep(Duration::from_millis(750));
    }

    /// Read until the first `AdmissionRejected`. Bounded, and a close or a read
    /// error fails rather than hangs.
    fn read_until_admission_rejected<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> ServerMessage {
        for _ in 0..2_000 {
            match socket.read().expect("read ws frame") {
                tungstenite::Message::Text(text) => {
                    let msg =
                        serde_json::from_str::<ServerMessage>(&text).expect("decode ServerMessage");
                    if matches!(msg, ServerMessage::AdmissionRejected { .. }) {
                        return msg;
                    }
                }
                tungstenite::Message::Close(_) => panic!("socket closed before any refusal"),
                _ => {}
            }
        }
        panic!("no admission refusal arrived within the frame budget");
    }

    /// Read until the venue's order-book snapshot for `request_id`.
    fn read_until_order_snapshot<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> mogwai_protocol::OrderStatusSnapshot {
        for _ in 0..2_000 {
            match socket.read().expect("read ws frame") {
                tungstenite::Message::Text(text) => {
                    if let ServerMessage::OrderStatusSnapshot(snapshot) =
                        serde_json::from_str::<ServerMessage>(&text).expect("decode ServerMessage")
                    {
                        return snapshot;
                    }
                }
                tungstenite::Message::Close(_) => panic!("socket closed before the snapshot"),
                _ => {}
            }
        }
        panic!("no order snapshot arrived within the frame budget");
    }

    /// Read a submit's batch out to its trailing `AccountState`, asserting the
    /// order was accepted rather than refused on the way. Draining the batch is
    /// what returns its held-budget charge.
    fn read_until_account_state<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
        client_order_id: &str,
    ) {
        for _ in 0..2_000 {
            match socket.read().expect("read ws frame") {
                tungstenite::Message::Text(text) => {
                    match serde_json::from_str::<ServerMessage>(&text)
                        .expect("decode ServerMessage")
                    {
                        // The batch's trailing frame has reached this client,
                        // but the venue releases its charge when the WRITER
                        // drops it - a hair after its `send` returns, which is
                        // concurrent with this read. Against a budget sized at
                        // exactly one command that hair is the difference
                        // between admitted and refused, so let the writer finish
                        // rather than racing it.
                        ServerMessage::AccountState(_) => {
                            std::thread::sleep(Duration::from_millis(150));
                            return;
                        }
                        ServerMessage::AdmissionRejected {
                            subject, reason, ..
                        } => panic!(
                            "{client_order_id} was refused for capacity - the budget did \
                             not come back: {subject:?} {reason}"
                        ),
                        _ => {}
                    }
                }
                tungstenite::Message::Close(_) => panic!("socket closed mid-batch"),
                _ => {}
            }
        }
        panic!("no AccountState arrived for {client_order_id}");
    }

    /// How a connection ended, from the client's side.
    #[derive(Debug, PartialEq, Eq)]
    enum PeerEnd {
        /// A websocket close frame arrived - the reasoned exit.
        Close,
        /// The transport ended with no close frame: the venue dropped the
        /// socket.
        Dropped,
        /// Neither, within the socket's read timeout: the venue is still holding
        /// the connection open.
        StillOpen,
    }

    /// Drain a socket until it ends, reporting how. Requires a read timeout on
    /// the underlying stream, or `StillOpen` would hang instead of being
    /// reported.
    fn drain_until_end<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> PeerEnd {
        for _ in 0..200_000 {
            match socket.read() {
                Ok(tungstenite::Message::Close(_)) => return PeerEnd::Close,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return PeerEnd::StillOpen;
                }
                Err(_) => return PeerEnd::Dropped,
            }
        }
        PeerEnd::StillOpen
    }

    /// Read until the venue closes, returning its close frame. `None` means the
    /// socket ended without one.
    fn read_until_close<S: std::io::Read + std::io::Write>(
        socket: &mut tungstenite::WebSocket<S>,
    ) -> Option<tungstenite::protocol::frame::CloseFrame> {
        for _ in 0..200_000 {
            match socket.read() {
                Ok(tungstenite::Message::Close(frame)) => return frame,
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    }

    #[test]
    fn reconcile_subscribe_start_ts_clamps_future_to_live() {
        // The clamp itself, without a socket: a future start returns None (a live
        // stream from the clock), while an in-range start passes through unchanged.
        let state = state();
        let sim_now = sim_now_ns(state.sim);
        let (clamped, issue) = reconcile_entry_start_ts(
            Some(sim_now + 3_600_000_000_000),
            state.data_origin_ns,
            sim_now,
        );
        assert_eq!(clamped, None, "a future start clamps to a live stream");
        assert!(matches!(
            issue,
            Some(SubscriptionIssue::StartAfterSimNow { .. })
        ));
        let (in_range, issue) =
            reconcile_entry_start_ts(Some(sim_now), state.data_origin_ns, sim_now);
        assert_eq!(
            in_range,
            Some(sim_now),
            "an in-range start is honored as given"
        );
        assert_eq!(issue, None);
    }

    #[test]
    fn per_entry_start_ts_positions_each_symbol() {
        let (btc_start, btc_issue) = reconcile_entry_start_ts(Some(200), 100, 1_000);
        let (eth_start, eth_issue) = reconcile_entry_start_ts(Some(800), 100, 1_000);
        assert_eq!(btc_start, Some(200));
        assert_eq!(eth_start, Some(800));
        assert_eq!(btc_issue, None);
        assert_eq!(eth_issue, None);
    }

    #[test]
    fn session_error_message_renders_the_sum_sentinel_honestly() {
        // F14: the usize::MAX sentinel marks a whole-array sum violation with no
        // single bad element. It must NOT print as intensity_hour[18446744073709551615].
        let sum = session_error_message(
            "BTCUSDT",
            mogwai_data::SessionProfileError {
                field: "intensity_hour",
                index: usize::MAX,
            },
        );
        assert!(sum.contains("does not sum"), "sum message: {sum}");
        assert!(
            !sum.contains("18446744073709551615"),
            "the raw sentinel must not leak: {sum}"
        );
        // A genuine per-element failure keeps the indexed message.
        let element = session_error_message(
            "BTCUSDT",
            mogwai_data::SessionProfileError {
                field: "vol_hour",
                index: 7,
            },
        );
        assert!(
            element.contains("vol_hour[7]"),
            "element message: {element}"
        );
    }

    #[test]
    fn validate_instrument_def_rejects_empty_symbol() {
        // F15: an empty symbol must be a clean startup error, not a downstream
        // generator `.expect` panic. base/quote are non-empty so the symbol check
        // is what fires.
        let mut def = mogwai_protocol::default_instruments()
            .into_iter()
            .next()
            .expect("a default instrument");
        def.symbol = String::new();
        let err = validate_instrument_def(&def)
            .expect_err("an empty symbol is rejected")
            .to_string();
        assert!(err.contains("symbol"), "the error names the symbol: {err}");
    }

    #[tokio::test]
    async fn quiesce_survives_a_panicking_fanout_task() {
        // S15: a fanout task that panics mid-stream must be awaited (the panic
        // logged, not swallowed) without propagating into the connection
        // teardown.
        let cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = Arc::clone(&cancel);
        let task = tokio::spawn(async move {
            while !task_cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            panic!("fanout task blew up");
        });
        // Returns cleanly despite the panic; a failure would propagate it and
        // fail the test.
        quiesce_subscription(Subscription {
            cancel,
            cancel_wake: Arc::new(tokio::sync::Notify::new()),
            task,
            last_sent_ts: Arc::new(AtomicU64::new(NO_TICK_SENT)),
        })
        .await;
    }

    #[test]
    fn panic_message_extracts_str_and_string_payloads() {
        // S15 relies on this to surface a joined tape-thread panic in the reaper.
        let str_panic = std::panic::catch_unwind(|| panic!("boom")).unwrap_err();
        assert_eq!(crate::tape::panic_message(&*str_panic), "boom");
        let string_panic =
            std::panic::catch_unwind(|| panic!("{}", String::from("dynamic"))).unwrap_err();
        assert_eq!(crate::tape::panic_message(&*string_panic), "dynamic");
    }

    #[test]
    fn heartbeat_period_is_floored_against_a_speed_flood() {
        // S21: a large speed collapses the scaled wall period; the floor keeps it
        // at MIN_HEARTBEAT_WALL rather than a per-socket kHz flood.
        let fast = SimClock {
            sim_epoch_ns: 1,
            wall_anchor_ns: 0,
            speed: 1e12,
        };
        assert!(heartbeat_period(1, fast) >= MIN_HEARTBEAT_WALL);
        // Above the floor, the configured cadence is preserved on the identity map.
        assert_eq!(
            heartbeat_period(250, SimClock::identity()),
            Duration::from_millis(250)
        );
    }

    #[test]
    fn account_state_is_delayed_as_an_execution_event() {
        // The `DelayAcks` exec-pump path delays everything `is_execution_event`
        // accepts. `AccountState` is an account/execution event, so it must be
        // delayed alongside fills and order-lifecycle events - not treated as
        // market data the way trades and quotes are. This is the server side of
        // the shared `ServerMessage::category` classification; the adapter buckets
        // the same frame into exec latency from the same source of truth.
        let account = ServerMessage::AccountState(mogwai_protocol::AccountState {
            account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
            balances: Vec::new(),
            positions: Vec::new(),
            ts_event: 1,
        });
        assert!(
            is_execution_event(&account),
            "AccountState rides the execution-delay path"
        );

        let trade = ServerMessage::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: "1".parse().expect("decimal"),
            size: "1".parse().expect("decimal"),
            aggressor: mogwai_protocol::AggressorSide::NoAggressor,
            ts_event: 1,
        });
        assert!(
            !is_execution_event(&trade),
            "trades are market data, not delayed by DelayAcks"
        );
    }

    #[test]
    fn stall_data_classifier_leaves_execution_and_heartbeat_alive() {
        let trade = ServerMessage::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: "1".parse().expect("decimal"),
            size: "1".parse().expect("decimal"),
            aggressor: mogwai_protocol::AggressorSide::NoAggressor,
            ts_event: 1,
        });
        assert!(trade.is_market_data());
        assert!(!is_execution_event(&trade));

        let accepted = ServerMessage::OrderAccepted {
            client_order_id: "O".into(),
            venue_order_id: "V".into(),
            ts_event: 1,
        };
        assert!(!accepted.is_market_data());
        assert!(is_execution_event(&accepted));

        let account = ServerMessage::AccountState(mogwai_protocol::AccountState {
            account_id: mogwai_protocol::AccountId::parse("MOGWAI-001").unwrap(),
            balances: Vec::new(),
            positions: Vec::new(),
            ts_event: 1,
        });
        assert!(!account.is_market_data());
        assert!(is_execution_event(&account));

        let heartbeat = ServerMessage::Heartbeat { ts_event: 1 };
        assert_eq!(heartbeat.category(), mogwai_protocol::EventKind::Data);
        assert!(!heartbeat.is_market_data());
        assert!(matches!(heartbeat, ServerMessage::Heartbeat { .. }));
    }

    #[test]
    fn window_until_ns_saturates_and_preserves_zero_window() {
        let now = 1_000_000_000;
        assert_eq!(window_until_ns(now, 0), now);
        assert_eq!(window_until_ns(now, 250), 1_250_000_000);
        assert_eq!(window_until_ns(now, u64::MAX), u64::MAX);
    }

    /// Pins the E.6 seam fix: an explicit `start_ts` is a deliberate
    /// historical/resume request and must be honored exactly, never nudged by
    /// a floor meant only for the no-`start_ts` live-continuation case.
    #[test]
    fn resume_seek_target_honors_explicit_start_ts() {
        assert_eq!(resume_seek_target(Some(100), Some(50)), Some(100));
        assert_eq!(resume_seek_target(Some(100), None), Some(100));
    }

    /// A live continuation (`start_ts: None`) with no in-flight predecessor for
    /// this symbol has nothing to resume past, so it falls through unchanged -
    /// `build_live_source` seeks its own freshly-sampled `sim_now`.
    #[test]
    fn resume_seek_target_passes_through_with_no_predecessor() {
        assert_eq!(resume_seek_target(None, None), None);
    }

    /// The seam this closes: a quiesced predecessor's last successfully-sent
    /// tick sits at `floor`. Seeking exactly at `floor` would re-deliver that
    /// tick (a duplicate); seeking to any freshly-sampled `sim_now` ahead of
    /// `floor + 1` would skip whatever the predecessor never got a chance to
    /// send (a gap). `floor + 1`, unconditionally, is the only target that
    /// does neither - it does not even consult `sim_now`.
    #[test]
    fn resume_seek_target_resumes_past_a_quiesced_predecessor() {
        assert_eq!(resume_seek_target(None, Some(500)), Some(501));
    }

    #[tokio::test]
    async fn http_orders_route_rejects_subscription_messages() {
        let err = submit_order_http(
            State(state()),
            account_headers(),
            Json(ClientMessage::Subscribe {
                subscriptions: subscribe_entries(["BTCUSDT"], None),
            }),
        )
        .await
        .expect_err("subscribe rejected");

        assert_eq!(err, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn generated_history_default_limit_is_bounded_and_fast() {
        let profiles = default_profiles();
        let start = Instant::now();
        let ticks = bounded_trades(
            "BTCUSDT",
            None,
            None,
            MAX_HISTORY_LIMIT,
            None,
            &profiles,
            TEST_DATA_ORIGIN,
        );
        let elapsed = start.elapsed();

        println!("default /trades synthesis elapsed: {elapsed:?}");
        assert_eq!(ticks.len(), MAX_HISTORY_LIMIT);
        assert!(ticks.len() <= 1_000);
        assert!(
            elapsed < Duration::from_millis(250),
            "default /trades synthesis took {elapsed:?}"
        );
    }

    #[test]
    fn generated_history_is_replayable_and_cursorable() {
        let profiles = default_profiles();
        let first = bounded_trades("BTCUSDT", None, None, 8, None, &profiles, TEST_DATA_ORIGIN);
        let replay = bounded_trades("BTCUSDT", None, None, 8, None, &profiles, TEST_DATA_ORIGIN);
        assert_eq!(trade_signatures(&first), trade_signatures(&replay));

        let cursor = first.last().expect("first page has trades").ts_event;
        let second = bounded_trades(
            "BTCUSDT",
            Some(cursor),
            None,
            8,
            None,
            &profiles,
            TEST_DATA_ORIGIN,
        );

        assert_eq!(
            second.first().expect("second page has trades").ts_event,
            cursor
        );
        assert!(second.iter().skip(1).all(|trade| trade.ts_event > cursor));
        assert_ne!(trade_signatures(&first), trade_signatures(&second));
    }

    // Landing 2 #13 regression pin: a forward warmup window that sits ON the tape
    // (inside `[data_origin, sim_now]`) yields bars. Before the boot-derived
    // origin, this seek ran from a frozen 2023 anchor to a 2026 window, drained
    // the cap, and returned an empty `200` the warmup could never complete.
    #[tokio::test]
    async fn warmup_window_on_tape_returns_bars() {
        let sim_now = TEST_DATA_ORIGIN + 86_400_000_000_000; // origin + 24h
        let mut state = state();
        state.data_origin_ns = TEST_DATA_ORIGIN;

        let start = sim_now - 3_600_000_000_000; // sim_now - 1h
        let query = HistoryQuery {
            symbol: "BTCUSDT".to_string(),
            start: Some(start),
            end: Some(sim_now),
            limit: None,
            regime: None,
        };
        let Json(ticks) = trades(State(state), axum::extract::Query(query))
            .await
            .expect("on-tape warmup is served, not refused");

        assert!(!ticks.is_empty(), "on-tape warmup window must yield bars");
        assert!(
            ticks
                .iter()
                .all(|t| t.ts_event >= start && t.ts_event <= sim_now),
            "every bar lands inside the requested window"
        );
    }

    // The straddle refusal: a `start` before `data_origin` can never be served, so
    // the handler returns `422` naming the floor rather than draining the cap to an
    // empty `200` the warmup cannot distinguish from "no trades happened".
    #[tokio::test]
    async fn trades_before_data_origin_refuses() {
        let mut state = state();
        state.data_origin_ns = TEST_DATA_ORIGIN;

        let before = TEST_DATA_ORIGIN - 3_600_000_000_000; // 1h before the origin
        let query = HistoryQuery {
            symbol: "BTCUSDT".to_string(),
            start: Some(before),
            end: None,
            limit: None,
            regime: None,
        };
        let (status, body) = trades(State(state), axum::extract::Query(query))
            .await
            .expect_err("an off-tape window is refused");

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body.contains(&TEST_DATA_ORIGIN.to_string()),
            "the refusal names the data_origin floor: {body}"
        );
    }

    // The ceiling refusal, symmetric with the origin floor: a `start` past
    // sim-now asks for tape that does not exist yet. Unfixed, the handler
    // extended the shared index into the future and served deterministic
    // FUTURE ticks - a look-ahead leak.
    #[tokio::test]
    async fn trades_beyond_sim_now_refuses() {
        let state = state(); // identity clock: sim-now is wall-now
        let future = now_ns() + 3_600_000_000_000; // 1h past the clock
        let query = HistoryQuery {
            symbol: "BTCUSDT".to_string(),
            start: Some(future),
            end: None,
            limit: None,
            regime: None,
        };
        let (status, body) = trades(State(state), axum::extract::Query(query))
            .await
            .expect_err("a future window is refused");

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body.contains("sim-now"),
            "the refusal names the sim-now ceiling: {body}"
        );
    }

    // The look-ahead clamp: a window whose tail crosses sim-now (here an `end`
    // a full hour into the future) is served only up to the clock. Unfixed,
    // the synthesis loop ground toward `end` and returned ticks stamped in
    // the future. The on-tape half spans six hours, not minutes: the
    // generator's ACD clustering realizes multi-hour thin deserts (a fixed
    // property of the symbol's seeded duration sequence, see bug D16), so a
    // narrow window sliding with wall-time boot instants sometimes lands in a
    // desert and catches zero trades - the wide window keeps the clamp under
    // test without asserting on tape density.
    #[tokio::test]
    async fn trades_window_is_clamped_at_sim_now() {
        let state = state();
        let start = now_ns().saturating_sub(6 * 3_600_000_000_000); // 6h of tape
        let end = now_ns() + 3_600_000_000_000; // 1h past the clock
        let query = HistoryQuery {
            symbol: "BTCUSDT".to_string(),
            start: Some(start),
            end: Some(end),
            limit: None,
            regime: None,
        };
        let Json(ticks) = trades(State(state), axum::extract::Query(query))
            .await
            .expect("the on-tape half of a straddling window is served");
        // Sampled after the handler sampled its own sim-now, so it bounds
        // every legitimately-served tick from above.
        let ceiling = now_ns();

        assert!(!ticks.is_empty(), "six hours of on-tape window has trades");
        assert!(
            ticks.iter().all(|t| t.ts_event <= ceiling),
            "no served tick may be stamped past sim-now"
        );
    }

    // A fresh live subscribe seeks to sim-now, NOT the data origin: the first live
    // tick lands at sim-now so there is no 24h backfill replayed before the stream
    // goes current (the identity-mode staleness the spec calls out).
    #[test]
    fn live_seek_starts_at_sim_now_identity() {
        let profiles = default_profiles();
        let data_origin = TEST_DATA_ORIGIN;
        let sim_now = TEST_DATA_ORIGIN + 86_400_000_000_000; // 24h of tape behind now
        let symbols = ["BTCUSDT".to_string()];

        let mut live =
            source::build_live_source(&symbols, None, None, &profiles, data_origin, sim_now)
                .expect("live source");
        let first = live.next_tick().expect("first live tick").ts_event();

        assert!(
            first >= sim_now,
            "first live tick {first} precedes sim_now {sim_now} - backfill leaked"
        );
        // The seek stops at sim-now, not hours past it: the first tick is within a
        // handful of inter-arrival gaps of the target.
        assert!(
            first - sim_now < 60_000_000_000,
            "first live tick {first} is more than 60s past sim_now {sim_now}"
        );
    }

    // The live stream continues the same realization a warmup window ends on: both
    // seek the one coherent tape (same seed, same data_origin) to sim-now, so a
    // history request for sim-now and the live first tick are byte-identical - no
    // price discontinuity at the warmup/live splice, no reconnect price reset.
    #[test]
    fn live_seek_is_continuous_with_history() {
        let profiles = default_profiles();
        let data_origin = TEST_DATA_ORIGIN;
        let sim_now = TEST_DATA_ORIGIN + 86_400_000_000_000;
        let symbols = ["BTCUSDT".to_string()];

        let mut hist =
            source::build_history_source("BTCUSDT", Some(sim_now), None, &profiles, data_origin)
                .expect("history source");
        let mut live =
            source::build_live_source(&symbols, None, None, &profiles, data_origin, sim_now)
                .expect("live source");

        let TickEvent::Trade(hist_first) = hist.next_tick().expect("history tick") else {
            panic!("generated source emits trades");
        };
        let TickEvent::Trade(live_first) = live.next_tick().expect("live tick") else {
            panic!("generated source emits trades");
        };

        assert_eq!(
            hist_first.ts_event, live_first.ts_event,
            "live resumes the same tape instant a warmup for sim_now ends on"
        );
        assert_eq!(
            hist_first.price, live_first.price,
            "price is contiguous across the warmup/live splice"
        );
    }

    #[test]
    fn out_of_range_regime_replays_clean() {
        let profiles = default_profiles();
        let seek = TEST_DATA_ORIGIN + 3_600_000_000_000; // 1h into the tape
        let clean = bounded_trades(
            "BTCUSDT",
            Some(seek),
            None,
            8,
            None,
            &profiles,
            TEST_DATA_ORIGIN,
        );
        let invalid =
            parse_history_regime(Some(r#"{"type":"LiquidityDrought","thin_factor":0.5}"#));
        assert_eq!(invalid, None);
        let fallback = bounded_trades(
            "BTCUSDT",
            Some(seek),
            None,
            8,
            invalid,
            &profiles,
            TEST_DATA_ORIGIN,
        );

        assert_eq!(trade_signatures(&clean), trade_signatures(&fallback));
    }

    #[test]
    fn subscribe_issue_reports_an_out_of_range_regime() {
        let (regime, dropped) =
            validate_regime_or_clean(Some(MarketRegime::LiquidityDrought { thin_factor: 0.5 }));

        assert_eq!(regime, None);
        assert!(dropped);
    }

    #[test]
    fn malformed_history_regime_replays_clean() {
        let regime = parse_history_regime(Some(r#"{"type":"Bogus"}"#));

        assert_eq!(regime, None);
    }

    #[test]
    fn reopen_gap_at_or_before_the_origin_is_stripped_and_reported() {
        // D3's API-boundary half: a gap the generator could never fire is
        // stripped at the boundary (returning its at_ts so the WS carrier can
        // announce it), while a fireable gap passes through untouched.
        let gap = |at_ts| MarketRegime::ReopenGap {
            at_ts,
            halt_secs: 60,
            gap_frac: 0.05,
        };

        let mut at_origin = Some(gap(TEST_DATA_ORIGIN));
        assert_eq!(
            strip_unfireable_reopen_gap(&mut at_origin, TEST_DATA_ORIGIN),
            Some(TEST_DATA_ORIGIN)
        );
        assert_eq!(at_origin, None, "the doomed gap is stripped to clean");

        let mut before_origin = Some(gap(TEST_DATA_ORIGIN - 1));
        assert_eq!(
            strip_unfireable_reopen_gap(&mut before_origin, TEST_DATA_ORIGIN),
            Some(TEST_DATA_ORIGIN - 1)
        );
        assert_eq!(before_origin, None);

        let mut fireable = Some(gap(TEST_DATA_ORIGIN + 1));
        assert_eq!(
            strip_unfireable_reopen_gap(&mut fireable, TEST_DATA_ORIGIN),
            None
        );
        assert_eq!(
            fireable,
            Some(gap(TEST_DATA_ORIGIN + 1)),
            "a fireable gap passes through untouched"
        );

        let mut other = Some(MarketRegime::VolStorm { vol_mult: 2.0 });
        assert_eq!(
            strip_unfireable_reopen_gap(&mut other, TEST_DATA_ORIGIN),
            None
        );
        assert_eq!(other, Some(MarketRegime::VolStorm { vol_mult: 2.0 }));
    }

    fn trade_signatures(trades: &[TradeTick]) -> Vec<(String, String, String, u64)> {
        trades
            .iter()
            .map(|trade| {
                (
                    trade.symbol.clone(),
                    trade.price.to_string(),
                    trade.size.to_string(),
                    trade.ts_event,
                )
            })
            .collect()
    }

    #[test]
    fn dedup_symbols_sorts_and_dedups() {
        assert_eq!(
            dedup_symbols(vec!["B".into(), "A".into(), "B".into()]),
            vec!["A".to_string(), "B".to_string()],
        );
    }

    #[test]
    fn normalize_limit_defaults_and_clamps() {
        assert_eq!(normalize_limit(None), MAX_HISTORY_LIMIT);
        assert_eq!(normalize_limit(Some(0)), 0);
        assert_eq!(normalize_limit(Some(10)), 10);
        assert_eq!(
            normalize_limit(Some(MAX_HISTORY_LIMIT + 5)),
            MAX_HISTORY_LIMIT
        );
    }

    #[test]
    fn zero_limit_yields_empty_page() {
        let profiles = default_profiles();
        assert!(
            bounded_trades(
                "BTCUSDT",
                None,
                None,
                normalize_limit(Some(0)),
                None,
                &profiles,
                TEST_DATA_ORIGIN,
            )
            .is_empty()
        );
    }

    /// Build a tape registry with a live reaper, for tests that drive tapes
    /// and fanout tasks directly rather than through a socket.
    fn tape_registry(cfg: &Config) -> Arc<crate::tape::TapeRegistry> {
        crate::tape::TapeRegistry::new(cfg, build_tape_permits(cfg)).0
    }

    fn tape_key(symbol: &str, data_origin_ns: u64, regime: Option<MarketRegime>) -> TapeKey {
        TapeKey {
            symbol: symbol.into(),
            data_origin_ns,
            regime: RegimeKey::from_regime(regime.as_ref()),
        }
    }

    fn tape_spawn(cfg: &Config, regime: Option<MarketRegime>, sim: SimClock) -> TapeSpawn {
        TapeSpawn {
            profiles: default_profiles(),
            regime,
            sim,
            speed: cfg.speed,
            gap_cap_ms: cfg.gap_cap_ms,
            fanout_depth: cfg.fanout_depth,
            zero_speed_stall_ms: cfg.zero_speed_stall_ms,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "a test builder for a struct whose fields are all explicit"
    )]
    fn fanout_spawn(
        symbol: &str,
        lease: crate::tape::TapeLease,
        target: Option<u64>,
        data_origin: u64,
        tx: mpsc::Sender<Outbound>,
        lanes: ExecLanes,
        diag_ticket: Option<Ticket>,
        cancel: Arc<AtomicBool>,
        cancel_wake: Arc<tokio::sync::Notify>,
        last_sent_ts: Arc<AtomicU64>,
    ) -> FanoutSpawn {
        FanoutSpawn {
            symbol: symbol.into(),
            generation: 0,
            lease,
            target,
            regime: None,
            profiles: default_profiles(),
            data_origin,
            tx,
            lanes,
            diag_ticket,
            cancel,
            cancel_wake,
            last_sent_ts,
            sim: SimClock::identity(),
            speed: 0.0,
            gap_cap_ms: 0,
        }
    }

    /// A tape streams into its subscribers and is cancellable + joinable. Pins
    /// E.7: once its last lease drops, the thread exits promptly and the reaper
    /// joins it clean (no detached thread left parked).
    #[tokio::test]
    async fn tape_is_cancellable_and_joins() {
        let cfg = Config {
            speed: 0.0,
            ..Config::default()
        };
        let registry = tape_registry(&cfg);
        // Origin at wall-now so the fresh seek to sim-now is trivial; this test
        // only checks the tape is live and reapable.
        let origin = now_ns();
        let mut lease = registry
            .attach(
                tape_key("BTCUSDT", origin, None),
                tape_spawn(&cfg, None, SimClock::identity()),
            )
            .expect("a permit");
        let frame = tokio::time::timeout(Duration::from_secs(3), lease.rx.recv())
            .await
            .expect("a frame arrives promptly")
            .expect("a frame");
        assert!(frame.ts_event > 0);
        drop(lease);
        tokio::time::timeout(Duration::from_secs(3), async {
            while registry.live_tapes() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the tape is reaped once its last lease drops");
    }

    /// A tape whose positioning seek exhausts its budget must report the dead
    /// stream on the wire, not die in silence. A regime'd tape never uses the
    /// checkpoint index (regime'd realizations are a different tape), so it
    /// takes the fresh from-origin drain; 60 days of tape at the committed
    /// cadence holds far more than MAX_HISTORY_SEEK_TICKS ticks, so the seek
    /// toward sim-now dies against the cap.
    ///
    /// Both attach seams are covered: a subscriber that attached while the seek
    /// was still in flight (cursor `Starting`) and one that attached after the
    /// tape poisoned get the identical answer, and the poisoned entry leaves the
    /// registry so a later subscribe retries the seek instead of inheriting it.
    #[tokio::test]
    async fn a_dead_seek_reports_seek_budget_exhausted() {
        let cfg = Config::default();
        let registry = tape_registry(&cfg);
        let regime = Some(MarketRegime::VolStorm { vol_mult: 2.0 });
        let origin = now_ns().saturating_sub(60 * 86_400_000_000_000);
        let key = tape_key("BTCUSDT", origin, regime);
        // Attached synchronously with the spawn, so this lease races the seek.
        let in_flight = registry
            .attach(key.clone(), tape_spawn(&cfg, regime, SimClock::identity()))
            .expect("a permit");
        let (tx, mut rx) = mpsc::channel::<Outbound>(8);
        let (lanes, mut lane_rx) = replay_lanes();
        let cancel = Arc::new(AtomicBool::new(false));
        let task = spawn_fanout(FanoutSpawn {
            regime,
            ..fanout_spawn(
                "BTCUSDT",
                in_flight,
                None,
                origin,
                tx,
                lanes.clone(),
                lanes.reserve_promise(),
                cancel,
                Arc::new(tokio::sync::Notify::new()),
                Arc::new(AtomicU64::new(NO_TICK_SENT)),
            )
        });
        // The diagnostic is admission truth, so it rides the PRIORITY lane -
        // ahead of held traffic, exempt from DelayAcks - not the market-data
        // channel, and it spends the promise reserved for this subscription.
        let ServerMessage::SubscriptionIssues { entries, .. } =
            tokio::time::timeout(Duration::from_secs(10), next_priority_frame(&mut lane_rx))
                .await
                .expect("the dead seek is reported")
        else {
            panic!("expected SubscriptionIssues")
        };
        assert!(matches!(
            entries.as_slice(),
            [mogwai_protocol::SubscriptionOutcome {
                generation: 0,
                symbol,
                issue: SubscriptionIssue::SeekBudgetExhausted,
            }] if symbol == "BTCUSDT"
        ));
        assert!(
            rx.recv().await.is_none(),
            "a dead stream sends no market data at all"
        );
        task.await.expect("the fanout task ends");

        // The poisoned entry is gone, so this attach re-seeks rather than
        // inheriting a dead one - and lands on the same verdict.
        tokio::time::timeout(Duration::from_secs(3), async {
            while registry.live_tapes() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a poisoned tape leaves the registry");
        let retried = registry
            .attach(key, tape_spawn(&cfg, regime, SimClock::identity()))
            .expect("a permit");
        assert!(matches!(
            retried.attach,
            crate::tape::CursorState::Starting | crate::tape::CursorState::Poisoned
        ));
    }

    /// A symbol the venue does not list has no source at all, so its tape
    /// poisons the same way a budget-exhausted seek does. Production never
    /// reaches this - the subscribe handler pre-filters unknown symbols - but
    /// the tape must not spin silently if that pre-filter ever regresses.
    #[tokio::test]
    async fn an_unlisted_symbol_poisons_its_tape() {
        let cfg = Config::default();
        let registry = tape_registry(&cfg);
        let lease = registry
            .attach(
                tape_key("FAKE", now_ns(), None),
                tape_spawn(&cfg, None, SimClock::identity()),
            )
            .expect("a permit");
        tokio::time::timeout(Duration::from_secs(3), async {
            while lease.tape.cursor_state() != crate::tape::CursorState::Poisoned {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("an unlisted symbol poisons its tape");
    }

    /// A subscription whose fanout task is parked behind a full connection
    /// channel still quiesces promptly. Drives the E.6 seam: the old stream is
    /// gone before the caller would spawn its replacement, and the park applies
    /// backpressure to this subscriber alone - never to the shared tape.
    #[tokio::test]
    async fn parked_fanout_quiesces_promptly() {
        let cfg = Config {
            speed: 0.0,
            ..Config::default()
        };
        let registry = tape_registry(&cfg);
        let origin = now_ns();
        let lease = registry
            .attach(
                tape_key("BTCUSDT", origin, None),
                tape_spawn(&cfg, None, SimClock::identity()),
            )
            .expect("a permit");
        // Capacity 1 and nothing draining, so the task parks in
        // `send_cancellable_async` almost immediately.
        let (tx, _rx) = mpsc::channel::<Outbound>(1);
        let (lanes, _lane_rx) = replay_lanes();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_wake = Arc::new(tokio::sync::Notify::new());
        let last_sent_ts = Arc::new(AtomicU64::new(NO_TICK_SENT));
        let task = spawn_fanout(fanout_spawn(
            "BTCUSDT",
            lease,
            None,
            origin,
            tx,
            lanes,
            None,
            Arc::clone(&cancel),
            Arc::clone(&cancel_wake),
            Arc::clone(&last_sent_ts),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        quiesce_subscription(Subscription {
            cancel,
            cancel_wake,
            task,
            last_sent_ts,
        })
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "parked fanout took {:?} to quiesce",
            started.elapsed(),
        );
    }

    /// The accelerated branch of the tape pacer deadline-paces against the sim
    /// clock instead of the legacy per-gap sleep. Anchoring the clock ~150 ms in
    /// the FUTURE makes the first tick's wall deadline `wall_ns(ts_event)` land
    /// at the anchor, so a correct deadline-pacer holds the very first tick for
    /// ~150 ms; a broken one (or the identity gap-pacer) would release it at
    /// once. The high speed collapses the generator's own inter-arrival gaps to
    /// near zero, isolating the assertion to the anchor delay alone. This is the
    /// only unit coverage of the deadline branch (the rest is the `--accelerated`
    /// smoke, which `brokkr check` does not run).
    #[tokio::test]
    async fn tape_deadline_paces_against_sim_clock() {
        const EPOCH: u64 = 1_900_000_000_000_000_000;
        let anchor = now_ns() + 150_000_000;
        let sim = SimClock {
            sim_epoch_ns: EPOCH,
            wall_anchor_ns: anchor,
            speed: 1_000.0,
        };
        // `gap_cap_ms`/`speed` are the identity-path knobs; under a non-identity
        // clock the tape ignores them and takes the deadline branch.
        let cfg = Config {
            speed: 1_000.0,
            gap_cap_ms: 1_000,
            ..Config::default()
        };
        let registry = tape_registry(&cfg);
        let started = Instant::now();
        // Origin AT the sim epoch, so the first tick lands there and its wall
        // deadline is the anchor - the delay this test measures.
        let mut lease = registry
            .attach(
                tape_key("BTCUSDT", EPOCH, None),
                tape_spawn(&cfg, None, sim),
            )
            .expect("a permit");
        let frame = tokio::time::timeout(Duration::from_secs(5), lease.rx.recv())
            .await
            .expect("a frame arrives")
            .expect("a frame");
        let elapsed = started.elapsed();
        // Deadline-paced to the future anchor (generous lower bound for slow CI).
        assert!(
            elapsed >= Duration::from_millis(80),
            "first tick released after {elapsed:?}, expected deadline pacing to the anchor",
        );
        // The generator anchored on the sim epoch, so stamps ride the sim axis.
        assert!(
            frame.ts_event >= EPOCH,
            "ts_event {} below sim epoch {EPOCH}",
            frame.ts_event
        );
    }

    /// A tape parked in its first pacing sleep (wall deadline 60s out via a
    /// future clock anchor, the same construction as
    /// `tape_deadline_paces_against_sim_clock`) must still be reaped promptly.
    /// Before the sliced cancel-aware sleep this join - and with it the reaper's
    /// whole queue - stalled for the full remaining gap.
    #[tokio::test]
    async fn tape_parked_in_deadline_pacing_reaps_promptly() {
        const EPOCH: u64 = 1_900_000_000_000_000_000;
        let sim = SimClock {
            sim_epoch_ns: EPOCH,
            wall_anchor_ns: now_ns() + 60_000_000_000,
            speed: 1_000.0,
        };
        let cfg = Config {
            speed: 1_000.0,
            gap_cap_ms: 1_000,
            ..Config::default()
        };
        let registry = tape_registry(&cfg);
        let lease = registry
            .attach(
                tape_key("BTCUSDT", EPOCH, None),
                tape_spawn(&cfg, None, sim),
            )
            .expect("a permit");
        // Warm the reaper's blocking pool so a first-use thread spawn is not
        // counted as the tape thread failing to exit.
        tokio::task::spawn_blocking(|| ())
            .await
            .expect("warm the reaper's blocking pool");
        // Let the thread reach the pacing sleep before dropping the lease, so
        // the prompt exit below is the sliced sleep observing the flag, not the
        // loop-top check winning a race.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let before_threads = thread_count();
        let started = Instant::now();
        drop(lease);
        assert_eq!(registry.live_tapes(), 0, "the entry leaves the map at once");
        // The join is what this measures: under an uninterruptible pacing sleep
        // the reaper would sit on this thread for the full 60s deadline.
        tokio::time::timeout(Duration::from_secs(5), async {
            while thread_count() >= before_threads {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a tape parked in deadline pacing is reaped promptly");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    fn thread_count() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .expect("status")
            .lines()
            .find_map(|line| line.strip_prefix("Threads:"))
            .expect("Threads field")
            .trim()
            .parse()
            .expect("thread count")
    }

    /// The resume floor must be read AFTER the fanout task has ended. This task
    /// models the interleaving the ordering closes: a task whose send is already
    /// past its cancel check when the resubscribe cancels it still completes
    /// that send and advances `last_sent_ts` before exiting. A floor loaded
    /// before the await misses that final tick (here it would read the
    /// NO_TICK_SENT sentinel and yield `None`), so the replacement re-serves it
    /// as a duplicate frame; loaded after, the value is final and the floor
    /// covers everything that reached the channel.
    #[tokio::test]
    async fn resume_floor_reads_last_sent_after_quiesce() {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_wake = Arc::new(tokio::sync::Notify::new());
        let last_sent_ts = Arc::new(AtomicU64::new(NO_TICK_SENT));
        let task_cancel = Arc::clone(&cancel);
        let task_last_sent = Arc::clone(&last_sent_ts);
        let task = tokio::spawn(async move {
            while !task_cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            // The in-flight send completes after cancel lands, exactly like a
            // reservation that resolved an instant before the flag was raised.
            task_last_sent.store(42, Ordering::Relaxed);
        });

        let floor = quiesce_and_resume_floor(Subscription {
            cancel,
            cancel_wake,
            task,
            last_sent_ts,
        })
        .await;

        assert_eq!(
            floor,
            Some(42),
            "the floor must include the tick sent while the quiesce was in flight"
        );
    }

    /// `DelayAcks` holds every execution event by ~ms from its own enqueue
    /// instant, not by ms times its queue position: a burst of three events
    /// (one submit's Accepted + Filled + AccountState) sleeps its windows
    /// concurrently, so the whole batch lands ~delay after enqueue instead of
    /// +delay/+2*delay/+3*delay off a serial per-dequeue sleep.
    #[tokio::test]
    async fn delay_acks_does_not_compound_across_queued_events() {
        const DELAY_MS: u64 = 100;
        let delay_ms = crate::accounts::AccountSlot::detached_for_tests();
        delay_ms.delay_ms.store(DELAY_MS, Ordering::Relaxed);
        let (exec_tx, exec_rx) = mpsc::unbounded_channel();
        let (tx, mut rx) = mpsc::channel::<Outbound>(8);
        let pump = spawn_exec_pump(exec_rx, delay_ms, SimClock::identity(), tx);

        let started = Instant::now();
        for i in 0..3u64 {
            exec_tx
                .send(HeldFrame {
                    arrived: started,
                    frame: OutboundFrame {
                        payload: Arc::from(
                            serde_json::to_string(&ServerMessage::OrderAccepted {
                                client_order_id: format!("O{i}"),
                                venue_order_id: format!("V{i}"),
                                ts_event: i,
                            })
                            .expect("serializes"),
                        ),
                        is_market_data: false,
                        charge: None,
                        slot: None,
                    },
                })
                .expect("enqueue exec event");
        }
        for _ in 0..3 {
            rx.recv().await.expect("delayed exec event");
        }
        let elapsed = started.elapsed();

        // The armed window must be honored at all (lower bound)...
        assert!(
            elapsed >= Duration::from_millis(DELAY_MS - 20),
            "batch released after {elapsed:?}, before the armed {DELAY_MS} ms window"
        );
        // ...but once per batch, not once per event: 3 serial sleeps would
        // take ~300 ms. Generous slack for slow CI, still far under 2x delay.
        assert!(
            elapsed < Duration::from_millis(2 * DELAY_MS),
            "batch took {elapsed:?}; per-dequeue sleeps compound the armed delay"
        );

        drop(exec_tx);
        pump.await.expect("pump drains cleanly");
    }

    /// A pacing sleep must observe cancellation within one poll slice, not
    /// after the full inter-tick wall gap: a 60s deadline here would hold the
    /// reap for a minute under a plain uninterruptible `thread::sleep`.
    #[test]
    fn pacing_sleep_returns_promptly_on_cancel() {
        let cancel = Arc::new(AtomicBool::new(false));
        let wall_anchor = now_ns();
        let instant_anchor = Instant::now();
        let target = wall_anchor + 60_000_000_000; // 60s of pacing
        let thread_cancel = Arc::clone(&cancel);
        let sleeper = std::thread::spawn(move || {
            crate::tape::sleep_until_wall_cancellable(
                target,
                wall_anchor,
                instant_anchor,
                &thread_cancel,
            );
        });

        std::thread::sleep(Duration::from_millis(50));
        cancel.store(true, Ordering::Relaxed);
        let cancelled_at = Instant::now();
        sleeper.join().expect("sleeper joins");

        assert!(
            cancelled_at.elapsed() < Duration::from_secs(1),
            "cancelled pacing sleep took {:?} to return",
            cancelled_at.elapsed()
        );
    }
}
