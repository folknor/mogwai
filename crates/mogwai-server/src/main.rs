//! mogwai fake-broker server.
//!
//! Hosts the native JSON-over-WS gateway (`/ws`) that the broadarrow adapter
//! connects to, plus an out-of-band control plane (`/control/divergence`) for
//! arming deterministic divergences from tests. The exchange logic lives in
//! [`mogwai_engine`]; market data is synthesized from the committed fingerprint
//! by [`mogwai_data`]; this binary owns sockets, the clock and replay pacing.

#[cfg(not(unix))]
compile_error!("mogwai-server requires a Unix target");

mod man;
mod source;

use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use clap::{Args, Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use mogwai_data::{GeneratorScalars, SessionProfile, TickEvent};
use mogwai_engine::Engine;
use mogwai_protocol::{
    AccountState, ClientMessage, InstrumentDef, MAX_HISTORY_LIMIT, MarketRegime, OrderType,
    QuoteTick, ServerClock, ServerMessage, SimClock, TradeTick, control::Divergence,
    validate_divergence, validate_market_regime, validate_modify_order, validate_submit_order,
};
use nix::{
    errno::Errno,
    fcntl::{Flock, FlockArg},
    sys::signal::{Signal, kill},
    unistd::{ForkResult, Pid},
};
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};

/// Replay/runtime configuration, loaded from a TOML config file at startup
/// (see `load`); never from ambient environment variables.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
struct Config {
    /// Simulated start instant. `0` keeps the identity wall-time clock.
    sim_epoch_ns: u64,
    /// Replay speed multiplier. `0.0` means unthrottled (stream as fast as the client
    /// drains). `1.0` is the default and paces to real wall-clock gaps; otherwise
    /// inter-tick wall delay = (tick gap) / speed.
    speed: f64,
    /// Maximum wall-clock sleep between two ticks under paced replay, in
    /// milliseconds. `0` disables the cap.
    gap_cap_ms: u64,
    /// Optional server-originated heartbeat cadence in milliseconds. `0`
    /// disables it. When enabled, each websocket session receives liveness
    /// frames that survive `StallData` but not `GoDark`.
    server_heartbeat_ms: u64,
    /// How far before sim-now the synthetic tape begins, in nanoseconds. The boot
    /// `data_origin = sim_now_at_boot - backfill_horizon_ns` is the earliest
    /// instant any source can serve, shared by every symbol's generator so the
    /// data timeline tracks the advertised clock (no frozen origin to drift past).
    /// A request straddling the floor is refused loudly rather than served short,
    /// so this default need not be exact - 24h covers a day's warmup. Documented
    /// next to `sim_epoch_ns`: both anchor the run's timelines.
    backfill_horizon_ns: u64,
    /// Optional explicit venue instrument set. When empty, the server seeds the
    /// protocol default set. When present, this is authoritative for
    /// `/instruments`, order validation, and data generation.
    #[serde(rename = "instrument")]
    instruments: Vec<ConfiguredInstrument>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sim_epoch_ns: 0,
            // Honest-by-default: wall-clock pace the generator's inter-arrival
            // gaps so a no-config server serves a realistic live feed, matching
            // the committed mogwai.toml. 0.0 remains available as an explicit
            // firehose for fast local iteration. Until the coherent simulated
            // clock lands this is the 1x baseline; afterwards it is the 1x point
            // of the acceleration axis.
            speed: 1.0,
            gap_cap_ms: 1000,
            server_heartbeat_ms: 0,
            // 24h: one day of warmup tape behind sim-now. A wrong horizon is made
            // loud (off-tape requests are refused, not silently under-served), so
            // the default's exactness is low-stakes.
            backfill_horizon_ns: 86_400_000_000_000,
            instruments: Vec::new(),
        }
    }
}

impl Config {
    /// Load run config from a TOML file. `path` is the parsed `--config <path>`
    /// argument when passed, otherwise `mogwai.toml` in the working directory.
    /// A missing file yields built-in defaults so the server still starts with
    /// no config present; a malformed file is a hard error rather than a silent
    /// fallback. Replaces the former MOGWAI_REPLAY_SPEED and MOGWAI_GAP_CAP_MS
    /// environment variables - run knobs belong in explicit input, not ambient
    /// environment.
    fn load(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let path = path.unwrap_or_else(|| PathBuf::from("mogwai.toml"));
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Clone, serde::Deserialize)]
struct ConfiguredInstrument {
    #[serde(flatten)]
    def: InstrumentDef,
    generator: GeneratorScalars,
    session: SessionProfile,
}

fn build_instrument_profiles(cfg: &Config) -> anyhow::Result<source::InstrumentProfiles> {
    if cfg.instruments.is_empty() {
        return Ok(source::InstrumentProfiles::defaults());
    }

    let fp = mogwai_data::Fingerprint::from_repo_json();
    let mut seen = HashSet::new();
    let mut profiles = Vec::with_capacity(cfg.instruments.len());

    for configured in &cfg.instruments {
        let def = &configured.def;
        validate_instrument_def(def)?;

        if !seen.insert(def.symbol.clone()) {
            anyhow::bail!("duplicate instrument symbol {}", def.symbol);
        }

        let mut scalars = configured.generator.clone();
        scalars.symbol = def.symbol.clone();
        if scalars.modal_tick != def.price_increment {
            anyhow::bail!(
                "instrument {} generator.modal_tick must equal price_increment",
                def.symbol
            );
        }
        if scalars.price_decimals != u32::from(def.price_precision) {
            anyhow::bail!(
                "instrument {} generator.price_decimals must equal price_precision",
                def.symbol
            );
        }
        scalars.validate(&fp).map_err(|err| {
            anyhow::anyhow!(
                "instrument {} generator.{} failed validation",
                def.symbol,
                err.field
            )
        })?;
        configured.session.validate().map_err(|err| {
            anyhow::anyhow!(
                "instrument {} session.{}[{}] must be finite and > 0",
                def.symbol,
                err.field,
                err.index
            )
        })?;

        profiles.push(source::InstrumentProfile::new(
            def.clone(),
            scalars,
            configured.session.clone(),
        ));
    }

    Ok(source::InstrumentProfiles::from_profiles(profiles))
}

fn validate_instrument_def(def: &InstrumentDef) -> anyhow::Result<()> {
    if def.symbol.trim().is_empty() {
        anyhow::bail!("instrument symbol must not be empty");
    }
    if def.base.trim().is_empty() {
        anyhow::bail!("instrument {} base must not be empty", def.symbol);
    }
    if def.quote.trim().is_empty() {
        anyhow::bail!("instrument {} quote must not be empty", def.symbol);
    }
    if def.price_increment <= Decimal::ZERO {
        anyhow::bail!("instrument {} price_increment must be positive", def.symbol);
    }
    if def.size_increment <= Decimal::ZERO {
        anyhow::bail!("instrument {} size_increment must be positive", def.symbol);
    }
    if !on_increment(
        def.price_increment,
        Decimal::new(1, u32::from(def.price_precision)),
    ) {
        anyhow::bail!(
            "instrument {} price_increment violates price_precision",
            def.symbol
        );
    }
    if !on_increment(
        def.size_increment,
        Decimal::new(1, u32::from(def.size_precision)),
    ) {
        anyhow::bail!(
            "instrument {} size_increment violates size_precision",
            def.symbol
        );
    }
    Ok(())
}

fn on_increment(value: Decimal, increment: Decimal) -> bool {
    increment > Decimal::ZERO && (value / increment).fract() == Decimal::ZERO
}

#[derive(Clone)]
struct AppState {
    engine: Arc<Mutex<Engine>>,
    cfg: Config,
    profiles: Arc<source::InstrumentProfiles>,
    /// Run clock shared with adapters over `/clock`.
    sim: SimClock,
    /// Earliest sim-time unix-ns instant the synthetic tape can serve, derived
    /// once at boot as `sim.sim_ns(now) - backfill_horizon_ns`. The binding floor
    /// for `/trades` and the anchor both source builders generate from, so the
    /// data origin and the advertised clock cannot diverge.
    data_origin_ns: u64,
    /// Execution-event delay in milliseconds, shared by all live writers.
    delay_ms: Arc<AtomicU64>,
    /// Sim-time unix-ns instant before which writers drop all outbound frames.
    dark_until_ns: Arc<AtomicU64>,
    /// Sim-time unix-ns instant before which writers drop market-data frames.
    stall_until_ns: Arc<AtomicU64>,
}

/// Nanoseconds since the Unix epoch - the server's clock, fed into the engine.
///
/// Thin local alias over [`mogwai_protocol::now_unix_nanos`], the shared
/// saturating clock reader the adapter also uses: a backward clock step
/// (NTP/leap) that puts `now` before the epoch saturates to 0 rather than
/// panicking every order path and divergence arm, and the `u128` nanosecond
/// count is clamped to `u64::MAX` rather than silently truncated. Kept as a
/// local name so the call sites below stay unchanged.
fn now_ns() -> u64 {
    mogwai_protocol::now_unix_nanos()
}

fn sim_now_ns(sim: SimClock) -> u64 {
    sim.sim_ns(now_ns())
}

fn sim_duration_from_millis(ms: u64) -> u64 {
    ms.saturating_mul(1_000_000)
}

/// Convert a millisecond window into an absolute sim-time unix-ns deadline.
fn window_until_ns(now: u64, ms: u64) -> u64 {
    now.saturating_add(ms.saturating_mul(1_000_000))
}

fn build_sim_clock(cfg: &Config, wall_anchor_ns: u64) -> anyhow::Result<SimClock> {
    if !cfg.speed.is_finite() {
        anyhow::bail!("speed must be finite");
    }
    if cfg.sim_epoch_ns == 0 {
        if cfg.speed != 1.0 && cfg.speed != 0.0 {
            anyhow::bail!("sim_epoch_ns must be set when speed is neither 0.0 nor 1.0");
        }
        return Ok(SimClock::identity());
    }
    if cfg.speed <= 0.0 {
        anyhow::bail!("speed must be > 0.0 when sim_epoch_ns is set");
    }
    Ok(SimClock {
        sim_epoch_ns: cfg.sim_epoch_ns,
        wall_anchor_ns,
        speed: cfg.speed,
    })
}

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
    Released,
    StillHeld(File),
}

const READY_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_KILL_GRACE: Duration = Duration::from_secs(2);
const PID_POLL_INTERVAL: Duration = Duration::from_millis(25);

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Man { topic } => {
            man::run(topic);
            Ok(())
        }
        Command::Stop(args) => stop(args),
        Command::Serve(args) => serve(args),
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

    let state = AppState {
        engine: Arc::new(Mutex::new(Engine::with_instruments(
            profiles.instrument_defs(),
        ))),
        cfg,
        profiles,
        sim,
        data_origin_ns,
        delay_ms: Arc::new(AtomicU64::new(0)),
        dark_until_ns: Arc::new(AtomicU64::new(0)),
        stall_until_ns: Arc::new(AtomicU64::new(0)),
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/account", get(account))
        .route("/instruments", get(instruments))
        .route("/trades", get(trades))
        .route("/quotes", get(quotes))
        .route("/clock", get(clock))
        .route("/orders", post(submit_order_http))
        .route("/ws", get(ws_upgrade))
        .route("/control/divergence", post(arm_divergence))
        .with_state(state);

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
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(pid_file)?;
    match try_lock_pid_file(file)? {
        LockAttempt::Acquired(mut lock) => {
            clear_pid_file(&mut lock)?;
            Ok(PidLockStatus::Acquired(lock))
        }
        LockAttempt::Held(mut file) => Ok(PidLockStatus::Held(read_pid_from_file(&mut file)?)),
    }
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
        LockAttempt::Acquired(_lock) => {
            ignore_error(std::fs::remove_file(&pid_file));
            println!("no daemon running (removed stale {})", pid_file.display());
            Ok(())
        }
        LockAttempt::Held(mut file) => {
            let pid = read_pid_when_ready(&mut file, STOP_TIMEOUT)?;
            signal_pid(pid, Signal::SIGTERM)?;
            match wait_for_lock_release(file, STOP_TIMEOUT)? {
                WaitLock::Released => {
                    ignore_error(std::fs::remove_file(&pid_file));
                    Ok(())
                }
                WaitLock::StillHeld(file) => {
                    signal_pid(pid, Signal::SIGKILL)?;
                    match wait_for_lock_release(file, STOP_KILL_GRACE)? {
                        WaitLock::Released => {
                            ignore_error(std::fs::remove_file(&pid_file));
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
            LockAttempt::Acquired(_lock) => return Ok(WaitLock::Released),
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

/// Control plane: arm a divergence to fire on its next trigger.
async fn arm_divergence(
    State(state): State<AppState>,
    Json(div): Json<Divergence>,
) -> impl IntoResponse {
    tracing::info!(?div, "arming divergence");
    // Validate at the arming boundary so an out-of-range knob (e.g. a
    // `PartialFillNext.fraction` outside `(0, 1]`) is rejected before it is
    // stored into server state or armed on the engine, rather than surfacing
    // as a degenerate fill downstream. Mirrors the `validate_market_regime`
    // gate on the subscription/history paths.
    if let Err(err) = validate_divergence(&div) {
        tracing::warn!(?div, err, "rejecting out-of-range divergence");
        return (StatusCode::BAD_REQUEST, err.to_string());
    }
    match div {
        Divergence::DelayAcks { ms } => {
            state.delay_ms.store(ms, Ordering::Relaxed);
        }
        Divergence::GoDark { ms } => {
            state.dark_until_ns.store(
                window_until_ns(sim_now_ns(state.sim), ms),
                Ordering::Relaxed,
            );
        }
        Divergence::StallData { ms } => {
            state.stall_until_ns.store(
                window_until_ns(sim_now_ns(state.sim), ms),
                Ordering::Relaxed,
            );
        }
        Divergence::ClearDivergences => {
            // Lift both server-owned temporal windows process-wide. `0` is the
            // cleared sentinel: `delay_ms == 0` skips the exec pump's delay sleep,
            // and `now_ns() < 0` is never true so the dark and data-stall
            // guards are off. There is no backlog to replay because gated
            // frames are dropped.
            state.delay_ms.store(0, Ordering::Relaxed);
            state.dark_until_ns.store(0, Ordering::Relaxed);
            state.stall_until_ns.store(0, Ordering::Relaxed);
        }
        // Server-ownership contract (pins B.4 / E.11): `DelayAcks`, `GoDark`,
        // `StallData`, and `ClearDivergences` are server-owned controls with no
        // synchronous engine-side trigger. The server owns them and must NEVER
        // forward them to `engine.arm()`.
        // The arms above intercept them before this catch-all, so `engine_div`
        // can only be one of the four engine-side variants. The assert makes a
        // future refactor that forwards a whole `HavocSpec.server` vec straight
        // to `engine.arm()` fail loudly rather than silently losing these knobs.
        engine_div => {
            debug_assert!(
                !matches!(
                    engine_div,
                    Divergence::DelayAcks { .. }
                        | Divergence::GoDark { .. }
                        | Divergence::StallData { .. }
                        | Divergence::ClearDivergences
                ),
                "DelayAcks/GoDark/StallData/ClearDivergences are server-owned and must not be forwarded to engine.arm()",
            );
            state.engine.lock().await.arm(engine_div);
        }
    }
    (StatusCode::ACCEPTED, String::new())
}

async fn instruments(State(state): State<AppState>) -> Json<Vec<InstrumentDef>> {
    Json(state.engine.lock().await.instrument_defs())
}

/// Pull route for the venue's current account snapshot.
///
/// AccountState is execution-owned and is otherwise only pushed with an order
/// event. An adapter connecting over either transport pulls this once so the
/// bridge's account row exists before the first order is worked, rather than
/// learning the account only when the first fill's AccountState arrives. The
/// route is transport-agnostic, so it also serves the HttpOrders execution
/// profile that opens no /ws socket and never sends Subscribe.
async fn account(State(state): State<AppState>) -> Json<AccountState> {
    let ts = sim_now_ns(state.sim);
    Json(state.engine.lock().await.account_snapshot(ts))
}

async fn clock(State(state): State<AppState>) -> Json<ServerClock> {
    // Publish the tape boundary alongside the affine map so a client can guard
    // its own warmup against `data_origin_ns` rather than issuing a doomed
    // off-tape fetch. `server_now_ns` is sampled here so the client gets sim-now
    // and the floor from one round trip, without reading its own (skewable) wall
    // clock. `data_origin_ns` is the boot-derived floor; the horizon is echoed so
    // the client can report the floor in its own terms.
    Json(ServerClock {
        sim: state.sim,
        server_now_ns: sim_now_ns(state.sim),
        data_origin_ns: state.data_origin_ns,
        backfill_horizon_ns: state.cfg.backfill_horizon_ns,
    })
}

/// A wire MARKET order carries no price (mirroring Nautilus, which never
/// stamps one), but the engine has no book of its own and fills "at the
/// order's own price" - so before either order-entry surface hands a
/// `SubmitOrder` to the engine, stamp a MARKET order missing a price with the
/// venue's own current synthesized price. A LIMIT order's price is left
/// untouched: the engine's "submit price required" rejection is correct for a
/// limit, it only over-reached for the price-less market case.
///
/// Synthesis (`positioned_generator` -> `source_at_or_before`) locks the
/// process-global checkpoint mutex and, on a cold/deep index, walks up to
/// `MAX_HISTORY_SEEK_TICKS`. `/trades` already pushes the identical synthesis
/// onto `spawn_blocking` rather than running it inline on the tokio worker
/// (see `trades` above); this does the same, so a burst of price-less market
/// orders cannot stall the runtime's worker pool or serialize behind a seeked
/// `/trades` request (or vice versa) any longer than the shared mutex itself
/// requires.
async fn stamp_market_price(msg: ClientMessage, state: &AppState) -> ClientMessage {
    let ClientMessage::SubmitOrder(mut order) = msg else {
        return msg;
    };
    if order.order_type == OrderType::Market && order.price.is_none() {
        let symbol = order.symbol.clone();
        let profiles = Arc::clone(&state.profiles);
        let data_origin = state.data_origin_ns;
        let sim_now = sim_now_ns(state.sim);
        order.price = match tokio::task::spawn_blocking(move || {
            source::current_price(&symbol, &profiles, data_origin, sim_now)
        })
        .await
        {
            Ok(price) => price,
            Err(e) => {
                tracing::error!(%e, "market price synthesis task failed");
                None
            }
        };
    }
    ClientMessage::SubmitOrder(order)
}

/// Run one order-entry command (`SubmitOrder`/`ModifyOrder`/`CancelOrder`)
/// through the protocol-boundary validators before it ever reaches the
/// engine, the same way `arm_divergence` calls `validate_divergence` and the
/// subscription/history paths call `validate_market_regime` before touching
/// server or engine state. A `SubmitOrder` with a non-positive quantity or a
/// priceless limit, or a `ModifyOrder` that sets neither field, is rejected
/// right here as a synthesized `OrderRejected`/`OrderModifyRejected` - never
/// reaching `stamp_market_price` or the engine mutex - instead of leaning on
/// the engine's own (correct, but redundant) defensive checks. `CancelOrder`
/// has no protocol-boundary validator and passes straight through. Shared by
/// both order-entry surfaces (`POST /orders` and the `/ws` handling below) so
/// the gate lives in exactly one place.
async fn process_order_cmd(order_cmd: ClientMessage, state: &AppState) -> Vec<ServerMessage> {
    let ts = sim_now_ns(state.sim);
    match &order_cmd {
        ClientMessage::SubmitOrder(order) => {
            if let Err(reason) = validate_submit_order(order) {
                return vec![ServerMessage::OrderRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: reason.to_string(),
                    ts_event: ts,
                }];
            }
        }
        ClientMessage::ModifyOrder {
            client_order_id,
            price,
            quantity,
        } => {
            if let Err(reason) = validate_modify_order(*price, *quantity) {
                return vec![ServerMessage::OrderModifyRejected {
                    client_order_id: client_order_id.clone(),
                    venue_order_id: None,
                    reason: reason.to_string(),
                    ts_event: ts,
                }];
            }
        }
        ClientMessage::CancelOrder { .. } => {}
        ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => {
            unreachable!("callers route Subscribe/Unsubscribe away before process_order_cmd")
        }
    }
    let order_cmd = stamp_market_price(order_cmd, state).await;
    state.engine.lock().await.process(order_cmd, ts)
}

/// HTTP order-entry surface for profiles that do not use `/ws` for orders.
async fn submit_order_http(
    State(state): State<AppState>,
    Json(msg): Json<ClientMessage>,
) -> Result<Json<Vec<ServerMessage>>, StatusCode> {
    match msg {
        ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => {
            Err(StatusCode::BAD_REQUEST)
        }
        order_cmd => Ok(Json(process_order_cmd(order_cmd, &state).await)),
    }
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    symbol: String,
    start: Option<u64>,
    end: Option<u64>,
    limit: Option<usize>,
    #[serde(default)]
    regime: Option<String>,
}

async fn trades(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> Result<Json<Vec<TradeTick>>, (StatusCode, String)> {
    let limit = normalize_limit(query.limit);
    let regime = parse_history_regime(query.regime.as_deref());
    let profiles = Arc::clone(&state.profiles);
    let data_origin = state.data_origin_ns;
    let HistoryQuery {
        symbol, start, end, ..
    } = query;

    // Analytic refuse of an off-tape window: a `start` before the data origin can
    // never be served (the tape begins at `data_origin`), so reject it LOUDLY with
    // the floor named rather than draining the seek cap and returning an empty
    // `200` the warmup cannot distinguish from "no trades happened". `None` means
    // "from origin" and is served; degenerate windows (start > end, limit 0) flow
    // through to `bounded_trades` unchanged.
    if let Some(start) = start
        && start < data_origin
    {
        let body = format!(
            "requested start {start} precedes data_origin_ns {data_origin}; the tape cannot serve before its origin"
        );
        tracing::warn!(start, data_origin, "refusing off-tape trades window");
        return Err((StatusCode::UNPROCESSABLE_ENTITY, body));
    }

    // Synthesizing up to `MAX_HISTORY_LIMIT` ticks is pure CPU work against the
    // generator (the source never blocks on IO), and `next_tick` never returns
    // `None`, so a no-`end` request always grinds out the full `limit`. Run it on
    // a blocking thread rather than inline on the tokio worker, so a burst of
    // `/trades` requests cannot stall the async runtime's worker pool.
    let ticks = match tokio::task::spawn_blocking(move || {
        bounded_trades(&symbol, start, end, limit, regime, &profiles, data_origin)
    })
    .await
    {
        Ok(ticks) => ticks,
        Err(e) => {
            tracing::error!(%e, "history synthesis task failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, String::new()));
        }
    };
    Ok(Json(ticks))
}

async fn quotes(
    axum::extract::Query(_query): axum::extract::Query<HistoryQuery>,
) -> Json<Vec<QuoteTick>> {
    // Mogwai's generated history is trades-only, so a bounded historical quote
    // fetch is empty by construction. If synthesized top-of-book is wired in,
    // this route grows the same seek-and-bound scan as `trades`.
    Json(Vec::new())
}

/// The single owner of `/trades` page-size policy: a missing `limit` requests a
/// full page, and any requested size is clamped to `MAX_HISTORY_LIMIT`. Folding
/// the `unwrap_or` + `.min()` here (rather than splitting the clamp into the
/// handler and an empty-vec guard into `bounded_trades`) keeps the page-size
/// contract in one place. A normalized `0` still flows through to the early
/// return in `bounded_trades`, which the synthesis loop relies on (the
/// `out.len() >= limit` break would otherwise yield one tick for `limit == 0`).
fn normalize_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(MAX_HISTORY_LIMIT).min(MAX_HISTORY_LIMIT)
}

fn bounded_trades(
    symbol: &str,
    start: Option<u64>,
    end: Option<u64>,
    limit: usize,
    regime: Option<MarketRegime>,
    profiles: &source::InstrumentProfiles,
    data_origin: u64,
) -> Vec<TradeTick> {
    if limit == 0 {
        return Vec::new();
    }

    let Some(mut merged) =
        source::build_history_source(symbol, start, regime, profiles, data_origin)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();

    while let Some(tick) = merged.next_tick() {
        if let Some(end) = end
            && tick.ts_event() > end
        {
            break;
        }

        let TickEvent::Trade(trade) = tick else {
            continue;
        };
        out.push(trade);
        if out.len() >= limit {
            break;
        }
    }

    out
}

fn parse_history_regime(raw: Option<&str>) -> Option<MarketRegime> {
    let raw = raw?;
    let Ok(regime) = serde_json::from_str::<MarketRegime>(raw) else {
        tracing::warn!(raw, "dropping malformed market regime");
        return None;
    };
    validate_regime_or_clean(Some(regime))
}

fn validate_regime_or_clean(regime: Option<MarketRegime>) -> Option<MarketRegime> {
    let regime = regime?;
    match validate_market_regime(&regime) {
        Ok(()) => Some(regime),
        Err(err) => {
            tracing::warn!(?regime, err, "dropping out-of-range market regime");
            None
        }
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// One client session.
///
/// The socket is split so order events and replayed market data can be written
/// concurrently: every outbound [`ServerMessage`] funnels through one mpsc
/// channel drained by a single writer task. Order commands are processed inline
/// against the engine; `Subscribe` spawns a replay feeding the same channel.
/// Execution events (the engine's own output) take one extra hop through a
/// dedicated delay pump before landing in that channel, so an armed
/// `DelayAcks` window paces only execution traffic - see `spawn_exec_pump`
/// for why that hop exists and for the per-event delay contract.
async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(1024);
    // One replay stream PER SYMBOL, not per sorted symbol-set. Keying per set let
    // overlapping subscriptions (`[A,B]` then `[B,C]`) spawn two independent
    // replays both emitting `B` from independent generators/clocks, so the client
    // saw duplicated, interleaved, out-of-order-per-symbol `B` trades - breaking
    // the ascending-`ts_event` ordering the adapter's `PollCursor` relies on
    // (E.5). With a per-symbol map a given symbol is fed by exactly one stream:
    // re-subscribing a symbol already in flight quiesces (cancels + joins) the old
    // stream before the replacement emits, so no stale tick interleaves at the
    // seam (E.6); the handles are tracked and reaped so threads cannot pile up
    // under connect/subscribe/disconnect churn (E.7).
    let mut replays: HashMap<String, Replay> = HashMap::new();
    let dark_until_ns = Arc::clone(&state.dark_until_ns);
    let stall_until_ns = Arc::clone(&state.stall_until_ns);
    let sim = state.sim;

    // `tx`/`rx` is the single channel the writer below drains and serializes
    // onto the socket - market data (replay threads, heartbeat) sends land in
    // it directly. Execution events take one extra hop through `exec_tx`/
    // `exec_pump` so a `DelayAcks` sleep never blocks the writer's own
    // `rx.recv()` loop (see `spawn_exec_pump`). Each event rides with the
    // `Instant` it was enqueued at, so the pump can anchor its delay deadline
    // at production time rather than at whenever it gets around to dequeuing.
    let (exec_tx, exec_rx) = mpsc::channel::<(Instant, ServerMessage)>(1024);

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let now = sim_now_ns(sim);
            if now < dark_until_ns.load(Ordering::Relaxed) {
                continue;
            }
            if msg.is_market_data() && now < stall_until_ns.load(Ordering::Relaxed) {
                continue;
            }
            // Skip an un-serializable frame rather than panicking the writer
            // task: this runs in a detached `tokio::spawn`, so an `expect` here
            // would silently tear down the whole connection's outbound stream.
            let payload = match serde_json::to_string(&msg) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(%e, "dropping un-serializable ServerMessage");
                    continue;
                }
            };
            if sink.send(Message::Text(payload.into())).await.is_err() {
                break; // client gone
            }
        }
    });

    let exec_pump = spawn_exec_pump(exec_rx, Arc::clone(&state.delay_ms), state.sim, tx.clone());

    let heartbeat = if state.cfg.server_heartbeat_ms > 0 {
        Some(spawn_heartbeat(
            state.cfg.server_heartbeat_ms,
            state.sim,
            tx.clone(),
        ))
    } else {
        None
    };

    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%e, %text, "undecodable client message");
                // Surface the decode failure on the wire instead of dropping it
                // silently: previously a malformed/under-specified frame (e.g.
                // `{"type":"Subscribe"}` missing `symbols`) looked identical to
                // a healthy-but-idle feed to the client. `tx.send` failing here
                // just means the writer task already gave up on this socket;
                // the outer loop's next read will observe the same and exit.
                drop(
                    tx.send(ServerMessage::ProtocolError {
                        reason: e.to_string(),
                        ts_event: sim_now_ns(state.sim),
                    })
                    .await,
                );
                continue;
            }
        };

        match client_msg {
            ClientMessage::Subscribe {
                symbols,
                start_ts,
                regime,
            } => {
                let regime = validate_regime_or_clean(regime);
                for symbol in dedup_symbols(symbols) {
                    // Quiesce any in-flight stream for this symbol BEFORE the
                    // replacement emits: cancel the old thread and join it (off
                    // the async worker) so it cannot land one last tick into the
                    // shared channel after the new generator starts. Without the
                    // join the old thread - blocked in a send or mid-`next_tick`
                    // - could deliver an out-of-order/duplicate tick at the seam
                    // (E.6). The join alone only stops the old thread from
                    // enqueuing anything ELSE, though - whatever it already
                    // enqueued before the cancel lands stays in the shared
                    // channel. `resume_floor` carries that thread's last
                    // successfully-sent `ts_event` (if it sent one) forward, so
                    // the replacement's own seek starts strictly past it instead
                    // of re-sampling a `sim_now` that could land at-or-before an
                    // already-delivered tick. The floor is read only AFTER the
                    // join (see `quiesce_and_resume_floor` for why the order is
                    // load-bearing).
                    let resume_floor = if let Some(old) = replays.remove(&symbol) {
                        quiesce_and_resume_floor(old).await
                    } else {
                        None
                    };
                    let cancel = Arc::new(AtomicBool::new(false));
                    let last_sent_ts = Arc::new(AtomicU64::new(NO_TICK_SENT));
                    let handle = spawn_replay(ReplaySpawn {
                        symbol: symbol.clone(),
                        start_ts,
                        regime,
                        speed: state.cfg.speed,
                        gap_cap_ms: state.cfg.gap_cap_ms,
                        profiles: Arc::clone(&state.profiles),
                        sim: state.sim,
                        data_origin: state.data_origin_ns,
                        tx: tx.clone(),
                        cancel: Arc::clone(&cancel),
                        resume_floor,
                        last_sent_ts: Arc::clone(&last_sent_ts),
                    });
                    replays.insert(
                        symbol,
                        Replay {
                            cancel,
                            handle,
                            last_sent_ts,
                        },
                    );
                }
            }
            ClientMessage::Unsubscribe { symbols } => {
                for symbol in dedup_symbols(symbols) {
                    if let Some(old) = replays.remove(&symbol) {
                        quiesce_replay(old).await;
                    }
                }
            }
            order_cmd => {
                let events = process_order_cmd(order_cmd, &state).await;
                // One arrival instant for the whole batch: the engine produced
                // these events together in one `process` call, so under an
                // armed `DelayAcks` they share one deadline and land together
                // ~ms late (see `spawn_exec_pump` for the per-event-deadline
                // contract this anchors).
                let arrived = Instant::now();
                for ev in events {
                    debug_assert!(
                        is_execution_event(&ev),
                        "order-entry events are always execution-category, never market data"
                    );
                    if exec_tx.send((arrived, ev)).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    // Cancel every replay first so the threads stop generating, then join them
    // (the writer task is still draining `rx`, so a thread blocked in a send
    // unblocks and observes the cancel promptly). Reaping the handles here means
    // a disconnect leaves no detached replay thread parked in `next_tick`/send
    // (E.7). Only after the threads are joined do we drop the last `tx` and let
    // the writer task finish.
    for replay in replays.values() {
        replay.cancel.store(true, Ordering::Relaxed);
    }
    for (_, replay) in replays.drain() {
        quiesce_replay(replay).await;
    }
    if let Some(handle) = heartbeat {
        handle.abort();
        if let Err(e) = handle.await
            && !e.is_cancelled()
        {
            tracing::warn!(%e, "heartbeat task did not shut down cleanly");
        }
    }
    // Abort rather than let the pump drain gracefully: the client is already
    // gone, so an execution event still sleeping out an armed `DelayAcks`
    // window has nowhere to go. This is what keeps a disconnect from lingering
    // for the rest of that sleep - the pump held the only remaining sleep in
    // this connection, so once it is torn down `writer.await` below returns
    // promptly instead of waiting out up to an hour of armed delay.
    exec_pump.abort();
    if let Err(e) = exec_pump.await
        && !e.is_cancelled()
    {
        tracing::warn!(%e, "exec delay pump did not shut down cleanly");
    }
    drop(tx);
    if let Err(e) = writer.await {
        tracing::warn!(%e, "writer task did not shut down cleanly");
    }
}

/// Delay queued execution events without head-of-line-blocking the market-data
/// feed behind them, honoring the `DelayAcks` contract per event.
///
/// The hop exists because the writer used to `sleep` inline on an execution
/// event before its next `rx.recv()`, which stalled every tick already queued
/// behind it for the entire armed `DelayAcks` window (up to one hour) - and,
/// on disconnect, kept the writer (and the connection's teardown) parked for
/// whatever remained of that sleep even though the socket was already gone.
/// This pump reads exec events (only ever sent here, via the paired sender,
/// by the order-entry handling in `handle_socket`) off their own channel and
/// forwards into `out`, so a long delay only holds up further exec traffic.
///
/// The delay deadline is PER EVENT, anchored at the instant the event was
/// enqueued: `arrival + armed delay`. Sleeping the full window between
/// consecutive dequeues would compound instead - a single submit produces
/// OrderAccepted + OrderFilled + AccountState, and under `DelayAcks ms` a
/// serial per-dequeue sleep delivered them at +ms, +2ms, +3ms while the
/// contract holds EVERY outbound execution event by ms. Anchoring each
/// deadline at arrival delivers all three ~ms late: an event whose deadline
/// already elapsed while a predecessor slept has no remaining wait and
/// forwards immediately. Arrival order is preserved (one `while` loop over
/// the channel) and arrival instants are monotone, so same-delay deadlines
/// are monotone too and the wire order matches the engine's event order
/// exactly as it did in the old inline path. The armed value is read per
/// event at dequeue, so re-arming or `ClearDivergences` applies to everything
/// still queued; the window rides the sim axis via `wall_duration`, matching
/// every other ms-window control.
fn spawn_exec_pump(
    mut exec_rx: mpsc::Receiver<(Instant, ServerMessage)>,
    delay_ms: Arc<AtomicU64>,
    sim: SimClock,
    out: mpsc::Sender<ServerMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some((arrived, msg)) = exec_rx.recv().await {
            let delay = delay_ms.load(Ordering::Relaxed);
            if delay > 0 {
                // `deadline - now` phrased as a saturating remainder so a
                // deadline already in the past sleeps zero, and a pathological
                // `arrival + delay` can never overflow `Instant` arithmetic.
                let hold = sim.wall_duration(sim_duration_from_millis(delay));
                let remaining = hold.saturating_sub(arrived.elapsed());
                if !remaining.is_zero() {
                    tokio::time::sleep(remaining).await;
                }
            }
            if out.send(msg).await.is_err() {
                break; // writer gone
            }
        }
    })
}

/// Feed per-session server liveness frames into the same channel the writer
/// gates and serializes, keeping socket writes single-owned and ordered.
fn spawn_heartbeat(
    interval_ms: u64,
    sim: SimClock,
    tx: mpsc::Sender<ServerMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period = sim.wall_duration(sim_duration_from_millis(interval_ms));
        // `tokio::time::interval` fires its first tick immediately rather than
        // after one period, so a plain `interval(period)` would emit a
        // `Heartbeat` at t=0 the instant the socket opens - before the client
        // has even sent its first `Subscribe`, and one full interval earlier
        // than every tick after it. `interval_at` with an explicit first
        // deadline one period out keeps the cadence uniform from the start.
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        loop {
            interval.tick().await;
            if tx
                .send(ServerMessage::Heartbeat {
                    ts_event: sim_now_ns(sim),
                })
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

/// A live per-symbol replay stream: the cancel flag the handler raises to stop
/// it, the OS-thread handle so the stream can be joined (reaped) rather than
/// detached and left to linger, and the last `ts_event` it successfully sent
/// (or [`NO_TICK_SENT`] if none yet) so a resubscribe of this symbol can seek
/// its replacement strictly past whatever this stream already delivered.
struct Replay {
    cancel: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
    last_sent_ts: Arc<AtomicU64>,
}

/// Sentinel `last_sent_ts` for a replay that has not yet sent a tick. A real
/// `ts_event` this large is not reachable within the tape's lifetime, so it is
/// distinguishable from any genuine timestamp without an `Option` wrapper.
const NO_TICK_SENT: u64 = u64::MAX;

/// Maximum wall time a replay thread parks while the outbound channel is full
/// before it re-checks its cancel flag. Bounds how long a cancelled stream can
/// stay parked in backpressure, so a quiesce/join completes promptly (E.7).
const REPLAY_SEND_POLL: Duration = Duration::from_millis(5);

/// Maximum wall time one slice of a pacing sleep runs before the replay
/// thread re-checks its cancel flag. Pacing deadlines are unbounded in
/// principle: the accelerated branch deadline-paces every tick with no cap
/// (a session-profile trough or a Subscribe with a future `start_ts` can put
/// the next deadline minutes to days out), and identity mode with
/// `gap_cap_ms = 0` (the documented "0 disables the cap") is just as
/// unbounded. `quiesce_replay` joins the thread inline in the connection's
/// read loop, so one uninterruptible `thread::sleep` that long would stall
/// every Unsubscribe/resubscribe/disconnect for the whole connection until
/// the sleep expired. Slicing the sleep bounds cancel-observation latency to
/// this constant, at a cost of at most ~50 wakeups/sec while pacing -
/// negligible against per-tick synthesis.
const REPLAY_SLEEP_POLL: Duration = Duration::from_millis(20);

/// Sleep until `target_wall_ns` - a unix-ns wall deadline mapped through the
/// replay's NTP-immune `(wall_anchor, instant_anchor)` pairing - in bounded
/// slices of at most [`REPLAY_SLEEP_POLL`], returning early the moment
/// `cancel` is raised. Both pacing branches of `spawn_replay` (accelerated
/// deadline pacing and identity gap pacing) sleep exclusively through this,
/// so a cancelled replay parked mid-gap wakes within one slice instead of
/// holding `quiesce_replay`'s join for the full inter-tick wall gap.
fn sleep_until_wall_cancellable(
    target_wall_ns: u64,
    wall_anchor: u64,
    instant_anchor: Instant,
    cancel: &AtomicBool,
) {
    if target_wall_ns <= wall_anchor {
        return;
    }
    let target = instant_anchor + Duration::from_nanos(target_wall_ns - wall_anchor);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if target <= now {
            return;
        }
        std::thread::sleep((target - now).min(REPLAY_SLEEP_POLL));
    }
}

/// Cancel a replay and join its thread, off the async worker.
///
/// The join can block briefly (until the thread observes the cancel between
/// generated ticks, within one [`REPLAY_SEND_POLL`] of a full-channel park, or
/// within one [`REPLAY_SLEEP_POLL`] slice of a pacing sleep), so it runs on a
/// blocking thread rather than stalling the tokio worker driving this
/// connection. Returning only once the thread has exited is what guarantees
/// quiescence: callers rely on it so a replaced stream cannot interleave a stale
/// tick after its successor begins (E.6), and so disconnect reaps every thread.
async fn quiesce_replay(replay: Replay) {
    replay.cancel.store(true, Ordering::Relaxed);
    if let Err(e) = tokio::task::spawn_blocking(move || replay.handle.join()).await {
        tracing::warn!(%e, "replay join task panicked");
    }
}

/// Quiesce a symbol's in-flight replay and return its resume floor: the last
/// `ts_event` it successfully sent, or `None` if it never sent one.
///
/// The floor is loaded only AFTER the join, and that ordering is load-bearing.
/// The replay thread's cancel checks bracket the send, but a send already past
/// its check completes and stores `last_sent_ts` even if `cancel` lands
/// mid-flight - so between a pre-join load and the thread's exit, one more
/// tick (T2) can enter the shared channel and advance `last_sent_ts` past the
/// loaded value (T1). A floor of T1 makes the replacement seek `T1 + 1` and
/// regenerate everything in `(T1, T2]`: duplicate frames on the wire, breaking
/// the ascending-`ts_event` ordering the adapter's cursor relies on - exactly
/// the E.5/E.6 seam the resume floor exists to close. Once the join returns
/// the thread has exited and `last_sent_ts` is final, so a floor read here
/// covers every tick that could ever have reached the channel.
async fn quiesce_and_resume_floor(old: Replay) -> Option<u64> {
    let last_sent_ts = Arc::clone(&old.last_sent_ts);
    quiesce_replay(old).await;
    let last_sent = last_sent_ts.load(Ordering::Relaxed);
    (last_sent != NO_TICK_SENT).then_some(last_sent)
}

/// Sort + dedup the requested symbols so a single subscription naming a symbol
/// twice does not spawn (then immediately quiesce) two streams for it.
fn dedup_symbols(mut symbols: Vec<String>) -> Vec<String> {
    symbols.sort();
    symbols.dedup();
    symbols
}

fn is_execution_event(msg: &ServerMessage) -> bool {
    // Delegates to the protocol's shared classifier (`ServerMessage::category`)
    // so the server's `DelayAcks` delay path and the adapter's inbound latency
    // bucketing decide exec-vs-data from one source of truth. Execution traffic
    // is everything but market data - notably `AccountState`, an account event
    // that reports balances and positions moved by fills, which both ends now
    // agree rides the execution path.
    msg.category().is_execution()
}

struct ReplaySpawn {
    symbol: String,
    start_ts: Option<u64>,
    regime: Option<MarketRegime>,
    speed: f64,
    gap_cap_ms: u64,
    profiles: Arc<source::InstrumentProfiles>,
    sim: SimClock,
    /// The boot-derived tape origin every generator anchors at.
    data_origin: u64,
    tx: mpsc::Sender<ServerMessage>,
    cancel: Arc<AtomicBool>,
    /// The predecessor stream's last successfully-sent `ts_event` for this
    /// symbol, when this spawn replaces one quiesced just now. `None` for a
    /// symbol with no in-flight predecessor (nothing to resume past).
    resume_floor: Option<u64>,
    /// Where this stream records its own last successfully-sent `ts_event`,
    /// read back by the handler if IT is quiesced by a future resubscribe.
    last_sent_ts: Arc<AtomicU64>,
}

/// The seek target a replay thread hands to `build_live_source`.
///
/// A live continuation (`start_ts: None`) that replaces a just-quiesced stream
/// for this symbol must not re-seek to a freshly-sampled `sim_now` unguarded:
/// quiescing only stops the OLD thread from enqueuing anything further, it
/// does not purge what it already enqueued, so a fresh `sim_now` seek could
/// land at-or-before that already-delivered tick (a duplicate) or skip
/// everything between it and `sim_now` (a gap). `resume_floor` is that
/// predecessor's last successfully-sent `ts_event`; seeking one nanosecond
/// past it - unconditionally, not clamped to a freshly-sampled `sim_now` -
/// resumes the exact same walk exactly where the predecessor left off,
/// rather than fast-forwarding past whatever it hadn't gotten to generate yet
/// (which a `.max(sim_now)` would silently skip). An explicit `start_ts` is a
/// deliberate historical/resume request from the client and is honored as
/// given, never adjusted by a floor meant for the no-`start_ts`
/// live-continuation case.
fn resume_seek_target(start_ts: Option<u64>, resume_floor: Option<u64>) -> Option<u64> {
    match (start_ts, resume_floor) {
        (Some(ts), _) => Some(ts),
        (None, Some(floor)) => Some(floor.saturating_add(1)),
        (None, None) => None,
    }
}

/// Stream generated trades for one symbol as market data into `tx`, returning
/// the joinable thread handle so the caller can reap it.
///
/// The replay runs on a dedicated OS thread. It applies backpressure by retrying
/// a `try_send` whenever the channel is full, sleeping at most
/// [`REPLAY_SEND_POLL`] between attempts and re-checking `cancel` each time, so a
/// stream blocked behind a slow/stalled client still observes cancellation
/// promptly instead of parking indefinitely in a plain `blocking_send` (E.7).
fn spawn_replay(spawn: ReplaySpawn) -> std::thread::JoinHandle<()> {
    let ReplaySpawn {
        symbol,
        start_ts,
        regime,
        speed,
        gap_cap_ms,
        profiles,
        sim,
        data_origin,
        tx,
        cancel,
        resume_floor,
        last_sent_ts,
    } = spawn;
    std::thread::spawn(move || {
        let symbols = [symbol.clone()];
        // Seek the shared tape to sim-now (or the resume cursor) instead of
        // re-anchoring a fresh generator behind now. Computed here, at the start of
        // the replay thread (~subscribe time), so a fresh subscribe's first live
        // tick lands at sim-now: no accelerated catch-up firehose, no identity
        // backfill replayed at real-time gaps, and a reconnect resumes the same
        // walk at its cursor rather than resetting the price to `start_price`.
        let sim_now = sim.sim_ns(now_ns());
        let seek_start_ts = resume_seek_target(start_ts, resume_floor);
        let Some(mut merged) = source::build_live_source(
            &symbols,
            seek_start_ts,
            regime,
            &profiles,
            data_origin,
            sim_now,
        ) else {
            return;
        };
        tracing::info!(%symbol, ?start_ts, sim_now, data_origin, ?regime, "replay started");

        // Every pacing deadline below is anchored to ONE monotonic `Instant`,
        // paired with ONE wall-clock read, taken here at thread start. Each
        // branch converts a wall-clock deadline (unix-nanos) into an offset from
        // this pairing and sleeps against the `Instant` rather than re-reading
        // the wall clock per tick - which is what makes pacing immune to an
        // NTP/leap step mid-session: nothing after this line consults
        // `CLOCK_REALTIME` again, so a backward step can no longer make the
        // feed stall (a huge `deadline - now`) and a forward step can no longer
        // make it burst (`deadline <= now` unpaced). The sleep itself is the
        // sliced, cancel-aware `sleep_until_wall_cancellable`, so an unbounded
        // pacing gap cannot hold a quiesce hostage; a sleep cut short by cancel
        // falls through to the pre-send cancel check below and exits before
        // emitting the tick it was pacing.
        let wall_anchor = now_ns();
        let instant_anchor = Instant::now();
        let sleep_until_wall = |target_wall_ns: u64| {
            sleep_until_wall_cancellable(target_wall_ns, wall_anchor, instant_anchor, &cancel);
        };

        // Identity pacing (the `else if speed > 0.0` branch below) sleeps the
        // inter-tick gap measured from the PREVIOUS tick; with no previous tick
        // the first one emits with no sleep. For a fresh subscribe - which seeks
        // the tape to sim-now - the first generated tick lands a few seconds PAST
        // sim-now, so an unpaced emit would hand the client a tick stamped ahead
        // of the clock. Seed the pacer with the seek target (sim-now) so the first
        // tick is paced to its own timestamp instead of racing out immediately.
        // The accelerated branch deadline-paces every tick (including the first)
        // and ignores `prev_ts`, so this only affects identity mode. An explicit
        // `start_ts` is a historical window or resume cursor that lands at or
        // before its target - that is backfill and should replay promptly, so it
        // keeps the `None` seed and the first tick emits at once.
        let mut prev_ts: Option<u64> = start_ts.is_none().then_some(sim_now);
        // Accumulates the identity/speed branch's absolute schedule as
        // `wall_anchor`-relative deadlines, rather than resetting a fresh
        // relative sleep off every tick. A per-tick relative sleep never
        // accounts for the time already spent generating/sending the previous
        // tick, so over a long session the realized spacing (gap + generation +
        // scheduling) would progressively lag real wall-clock time. Chaining
        // deadlines instead - each the last deadline plus this tick's wait -
        // removes that drift: a slow iteration simply arrives at an
        // already-past deadline and does not sleep, rather than pushing every
        // later deadline back by the overrun.
        let mut next_deadline_wall: Option<u64> = None;
        while let Some(tick) = merged.next_tick() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if !sim.is_identity() {
                sleep_until_wall(sim.wall_ns(tick.ts_event()));
            } else if speed > 0.0 {
                if let Some(prev) = prev_ts {
                    let gap_ns = tick.ts_event().saturating_sub(prev);
                    // Pace at nanosecond resolution: integer-dividing the scaled
                    // gap down to whole milliseconds collapses any sub-ms
                    // inter-tick gap to a zero-delay send, so micros-apart bursts
                    // (which the generator emits) wouldn't be paced at all and the
                    // realized timeline would not track original_timeline / speed.
                    let mut wait_ns = (gap_ns as f64 / speed) as u64;
                    if gap_cap_ms > 0 {
                        wait_ns = wait_ns.min(gap_cap_ms.saturating_mul(1_000_000));
                    }
                    let deadline = next_deadline_wall
                        .unwrap_or(wall_anchor)
                        .saturating_add(wait_ns);
                    sleep_until_wall(deadline);
                    next_deadline_wall = Some(deadline);
                }
                prev_ts = Some(tick.ts_event());
            }

            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let ts_event = tick.ts_event();
            let msg = match tick {
                TickEvent::Trade(t) => ServerMessage::Trade(t),
                TickEvent::Quote(q) => ServerMessage::Quote(q),
            };
            if send_cancellable(&tx, msg, &cancel).is_err() {
                break; // client gone or stream cancelled
            }
            last_sent_ts.store(ts_event, Ordering::Relaxed);
        }
        tracing::info!(%symbol, "replay finished");
    })
}

/// Send `msg` into `tx`, applying backpressure without parking indefinitely.
///
/// `Sender::blocking_send` would block until the channel drains, and the replay
/// thread would only re-check `cancel` at the next loop top - so a stream behind
/// a stalled client could sit unkillable in a full channel. Instead this retries
/// `try_send`, sleeping at most [`REPLAY_SEND_POLL`] when the channel is full and
/// re-checking `cancel` between attempts. `Err(())` means the stream should stop
/// (client gone, or cancelled while parked).
fn send_cancellable(
    tx: &mpsc::Sender<ServerMessage>,
    msg: ServerMessage,
    cancel: &AtomicBool,
) -> Result<(), ()> {
    use mpsc::error::TrySendError;
    let mut msg = msg;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(());
        }
        match tx.try_send(msg) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                msg = returned;
                std::thread::sleep(REPLAY_SEND_POLL);
            }
            Err(TrySendError::Closed(_)) => return Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::{OrderType, Side, SubmitOrder, TimeInForce};
    use std::time::Instant;

    // A fixed boot-derived origin for the synthesis tests: production derives it
    // from the clock at boot, but these tests pin a concrete instant so a
    // from-origin page and its resume cursor are reproducible.
    const TEST_DATA_ORIGIN: u64 = 1_700_438_400_000_000_000;

    fn default_profiles() -> Arc<source::InstrumentProfiles> {
        Arc::new(source::InstrumentProfiles::defaults())
    }

    fn state() -> AppState {
        let profiles = default_profiles();
        AppState {
            engine: Arc::new(Mutex::new(Engine::with_instruments(
                profiles.instrument_defs(),
            ))),
            cfg: Config {
                sim_epoch_ns: 0,
                speed: 0.0,
                gap_cap_ms: 0,
                server_heartbeat_ms: 0,
                backfill_horizon_ns: 86_400_000_000_000,
                instruments: Vec::new(),
            },
            profiles,
            sim: SimClock::identity(),
            // Coherent by construction, matching the boot formula: identity sim-now
            // is wall-now, so the tape origin sits 24h behind. A handler test that
            // needs a different floor overrides `data_origin_ns` after building.
            data_origin_ns: now_ns().saturating_sub(86_400_000_000_000),
            delay_ms: Arc::new(AtomicU64::new(0)),
            dark_until_ns: Arc::new(AtomicU64::new(0)),
            stall_until_ns: Arc::new(AtomicU64::new(0)),
        }
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
            let (mut socket, _) =
                tungstenite::connect(format!("ws://{addr}/ws")).expect("ws connect");
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

        assert!(
            matches!(frame, ServerMessage::ProtocolError { .. }),
            "expected ProtocolError, got {frame:?}"
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
            Json(ClientMessage::Subscribe {
                symbols: vec!["BTCUSDT".into()],
                start_ts: None,
                regime: None,
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
    fn subscribe_out_of_range_regime_is_dropped_to_clean() {
        let regime =
            validate_regime_or_clean(Some(MarketRegime::LiquidityDrought { thin_factor: 0.5 }));

        assert_eq!(regime, None);
    }

    #[test]
    fn malformed_history_regime_replays_clean() {
        let regime = parse_history_regime(Some(r#"{"type":"Bogus"}"#));

        assert_eq!(regime, None);
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

    /// A replay drains into the channel and is cancellable + joinable. Pins E.7:
    /// once cancelled, the thread exits promptly and the handle joins clean (no
    /// detached thread left parked).
    #[tokio::test]
    async fn replay_is_cancellable_and_joins() {
        let cfg = Config {
            sim_epoch_ns: 0,
            speed: 0.0,
            gap_cap_ms: 0,
            server_heartbeat_ms: 0,
            backfill_horizon_ns: 86_400_000_000_000,
            instruments: Vec::new(),
        };
        let profiles = default_profiles();
        let (tx, mut rx) = mpsc::channel::<ServerMessage>(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = spawn_replay(ReplaySpawn {
            symbol: "BTCUSDT".to_string(),
            start_ts: None,
            regime: None,
            speed: cfg.speed,
            gap_cap_ms: cfg.gap_cap_ms,
            profiles,
            sim: SimClock::identity(),
            // Origin at wall-now so the fresh-subscribe seek to sim-now is trivial;
            // this test only checks the stream is live and cancellable.
            data_origin: now_ns(),
            tx,
            cancel: Arc::clone(&cancel),
            resume_floor: None,
            last_sent_ts: Arc::new(AtomicU64::new(NO_TICK_SENT)),
        });
        // First tick arrives, confirming the stream is live and feeding the
        // default instrument.
        let first = rx.recv().await.expect("a tick");
        assert!(matches!(
            first,
            ServerMessage::Trade(_) | ServerMessage::Quote(_)
        ));

        // Cancel + join must complete: the thread, even if parked behind the
        // bounded channel, observes the flag within one send-poll and exits.
        quiesce_replay(Replay {
            cancel,
            handle,
            last_sent_ts: Arc::new(AtomicU64::new(NO_TICK_SENT)),
        })
        .await;
    }

    /// Re-subscribing a symbol whose stream is parked behind a full channel still
    /// quiesces promptly. Drives the E.6 seam: the old stream is gone before the
    /// caller would spawn its replacement.
    #[tokio::test]
    async fn parked_replay_quiesces_promptly() {
        let cfg = Config {
            sim_epoch_ns: 0,
            speed: 0.0,
            gap_cap_ms: 0,
            server_heartbeat_ms: 0,
            backfill_horizon_ns: 86_400_000_000_000,
            instruments: Vec::new(),
        };
        let profiles = default_profiles();
        // Capacity 1 so the generator fills the channel and parks in
        // `send_cancellable` almost immediately, never draining.
        let (tx, _rx) = mpsc::channel::<ServerMessage>(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = spawn_replay(ReplaySpawn {
            symbol: "BTCUSDT".to_string(),
            start_ts: None,
            regime: None,
            speed: cfg.speed,
            gap_cap_ms: cfg.gap_cap_ms,
            profiles,
            sim: SimClock::identity(),
            data_origin: now_ns(),
            tx,
            cancel: Arc::clone(&cancel),
            resume_floor: None,
            last_sent_ts: Arc::new(AtomicU64::new(NO_TICK_SENT)),
        });

        let started = Instant::now();
        quiesce_replay(Replay {
            cancel,
            handle,
            last_sent_ts: Arc::new(AtomicU64::new(NO_TICK_SENT)),
        })
        .await;
        // A few send-poll intervals of slack; a plain `blocking_send` would have
        // parked forever here because `_rx` never drains.
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "parked replay took {:?} to quiesce",
            started.elapsed(),
        );
    }

    /// The accelerated branch of `spawn_replay` deadline-paces against the sim
    /// clock instead of the legacy per-gap sleep. Anchoring the clock ~150 ms in
    /// the FUTURE makes the first tick's wall deadline `wall_ns(ts_event)` land
    /// at the anchor, so a correct deadline-pacer holds the very first tick for
    /// ~150 ms; a broken one (or the identity gap-pacer) would release it at
    /// once. The high speed collapses the generator's own inter-arrival gaps to
    /// near zero, isolating the assertion to the anchor delay alone. This is the
    /// only unit coverage of the deadline branch (the rest is the `--accelerated`
    /// smoke, which `brokkr check` does not run).
    #[tokio::test]
    async fn replay_deadline_paces_against_sim_clock() {
        const EPOCH: u64 = 1_900_000_000_000_000_000;
        let anchor = now_ns() + 150_000_000;
        let sim = SimClock {
            sim_epoch_ns: EPOCH,
            wall_anchor_ns: anchor,
            speed: 1_000.0,
        };
        // `gap_cap_ms`/`speed` are the identity-path knobs; under a non-identity
        // clock `spawn_replay` ignores them and takes the deadline branch.
        let cfg = Config {
            sim_epoch_ns: EPOCH,
            speed: 1_000.0,
            gap_cap_ms: 1_000,
            server_heartbeat_ms: 0,
            backfill_horizon_ns: 86_400_000_000_000,
            instruments: Vec::new(),
        };
        let profiles = default_profiles();
        let (tx, mut rx) = mpsc::channel::<ServerMessage>(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let handle = spawn_replay(ReplaySpawn {
            symbol: "BTCUSDT".to_string(),
            start_ts: Some(EPOCH),
            regime: None,
            speed: cfg.speed,
            gap_cap_ms: cfg.gap_cap_ms,
            profiles,
            sim,
            // Origin AT the seek target, so the first tick lands at EPOCH and its
            // wall deadline is the anchor - the delay this test measures.
            data_origin: EPOCH,
            tx,
            cancel: Arc::clone(&cancel),
            resume_floor: None,
            last_sent_ts: Arc::new(AtomicU64::new(NO_TICK_SENT)),
        });

        let first = rx.recv().await.expect("a tick");
        let elapsed = started.elapsed();
        let ts_event = match first {
            ServerMessage::Trade(t) => t.ts_event,
            ServerMessage::Quote(q) => q.ts_event,
            other => panic!("unexpected first frame: {other:?}"),
        };
        // Deadline-paced to the future anchor (generous lower bound for slow CI).
        assert!(
            elapsed >= Duration::from_millis(80),
            "first tick released after {elapsed:?}, expected deadline pacing to the anchor",
        );
        // The generator anchored on the sim epoch, so stamps ride the sim axis.
        assert!(
            ts_event >= EPOCH,
            "ts_event {ts_event} below sim epoch {EPOCH}"
        );

        quiesce_replay(Replay {
            cancel,
            handle,
            last_sent_ts: Arc::new(AtomicU64::new(NO_TICK_SENT)),
        })
        .await;
    }

    /// The resume floor must be read AFTER the old replay is joined. This
    /// thread models the interleaving the ordering closes: a replay whose
    /// send is already past its cancel check when the resubscribe cancels it
    /// still completes that send and advances `last_sent_ts` before exiting.
    /// A floor loaded before the join misses that final tick (here it would
    /// read the NO_TICK_SENT sentinel and yield `None`), so the replacement
    /// re-serves it as a duplicate frame; loaded after the join the value is
    /// final and the floor covers everything that reached the channel.
    #[tokio::test]
    async fn resume_floor_reads_last_sent_after_quiesce() {
        let cancel = Arc::new(AtomicBool::new(false));
        let last_sent_ts = Arc::new(AtomicU64::new(NO_TICK_SENT));
        let thread_cancel = Arc::clone(&cancel);
        let thread_last_sent = Arc::clone(&last_sent_ts);
        let handle = std::thread::spawn(move || {
            while !thread_cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            // The in-flight send completes after cancel lands, exactly like a
            // `try_send` that succeeded an instant before the flag was raised.
            thread_last_sent.store(42, Ordering::Relaxed);
        });

        let floor = quiesce_and_resume_floor(Replay {
            cancel,
            handle,
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
        let delay_ms = Arc::new(AtomicU64::new(DELAY_MS));
        let (exec_tx, exec_rx) = mpsc::channel::<(Instant, ServerMessage)>(8);
        let (tx, mut rx) = mpsc::channel::<ServerMessage>(8);
        let pump = spawn_exec_pump(exec_rx, delay_ms, SimClock::identity(), tx);

        let started = Instant::now();
        for i in 0..3u64 {
            exec_tx
                .send((
                    started,
                    ServerMessage::OrderAccepted {
                        client_order_id: format!("O{i}"),
                        venue_order_id: format!("V{i}"),
                        ts_event: i,
                    },
                ))
                .await
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
    /// join for a minute under a plain uninterruptible `thread::sleep`.
    #[test]
    fn pacing_sleep_returns_promptly_on_cancel() {
        let cancel = Arc::new(AtomicBool::new(false));
        let wall_anchor = now_ns();
        let instant_anchor = Instant::now();
        let target = wall_anchor + 60_000_000_000; // 60s of pacing
        let thread_cancel = Arc::clone(&cancel);
        let sleeper = std::thread::spawn(move || {
            sleep_until_wall_cancellable(target, wall_anchor, instant_anchor, &thread_cancel);
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

    /// End-to-end on the accelerated deadline branch: a replay parked in its
    /// first pacing sleep (wall deadline 60s out via a future clock anchor,
    /// the same construction as `replay_deadline_paces_against_sim_clock`)
    /// must still quiesce promptly. `quiesce_replay` joins the thread inline
    /// in the connection's read loop, so before the sliced cancel-aware sleep
    /// this join - and with it every Unsubscribe/resubscribe/disconnect on
    /// the connection - stalled for the full remaining gap.
    #[tokio::test]
    async fn replay_parked_in_deadline_pacing_quiesces_promptly() {
        const EPOCH: u64 = 1_900_000_000_000_000_000;
        let anchor = now_ns() + 60_000_000_000; // first tick's wall deadline 60s out
        let sim = SimClock {
            sim_epoch_ns: EPOCH,
            wall_anchor_ns: anchor,
            speed: 1_000.0,
        };
        let profiles = default_profiles();
        let (tx, _rx) = mpsc::channel::<ServerMessage>(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let last_sent_ts = Arc::new(AtomicU64::new(NO_TICK_SENT));
        let handle = spawn_replay(ReplaySpawn {
            symbol: "BTCUSDT".to_string(),
            start_ts: Some(EPOCH),
            regime: None,
            speed: 1_000.0,
            gap_cap_ms: 1_000,
            profiles,
            sim,
            // Origin AT the seek target so the source builds instantly and the
            // thread's first act is the 60s pacing sleep this test interrupts.
            data_origin: EPOCH,
            tx,
            cancel: Arc::clone(&cancel),
            resume_floor: None,
            last_sent_ts: Arc::clone(&last_sent_ts),
        });

        // Let the thread reach the pacing sleep before cancelling, so the
        // prompt return below is the sliced sleep observing the flag, not the
        // loop-top check winning a race.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = Instant::now();
        quiesce_replay(Replay {
            cancel,
            handle,
            last_sent_ts,
        })
        .await;

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "replay parked in deadline pacing took {:?} to quiesce",
            started.elapsed()
        );
    }
}
