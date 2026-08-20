// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! One foreground mogwai venue run: boot, bind, announce readiness, serve.
//!
//! This is the entry the `mogwai` binary's `serve` subcommand calls. The
//! argument parsing lives with the CLI; everything the venue actually DOES with
//! those three values lives here, next to the state it builds.

use std::{io::Write, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    routing::{get, post},
};

use crate::{
    config::{Config, build_instrument_profiles, build_run_clock, now_ns},
    http::{self, AppState, account, arm_divergence, clock, instruments, quotes, trades},
    long_version,
    ws::ws_upgrade,
    {run, source, sweeper},
};

/// Raw fills the generator synthesizes per wall second, used only to project
/// warmup cost at boot. It is a MEASURED number, not a target: see the
/// "warmup materialization throughput" section of `reference/performance.md`,
/// which records the boots it comes from and the method. Re-measure the same way
/// whenever the generator, the checkpoint stride or the tape protocol changes,
/// or the projection quietly drifts away from what boot actually costs.
///
/// It drifted exactly that way once. This read 5_000_000 while three measured
/// boots agreed on 2.9 M, so the projection ran 1.7x optimistic and the
/// `WARMUP_WARN_SECS` threshold below fired at roughly 104 seconds of real cost
/// rather than the 60 it names. The number covers the WHOLE boot interval -
/// walk, checkpoint retention and the frontier draw - because that is what an
/// operator actually waits through.
const SYNTHESIS_TICKS_PER_SEC: f64 = 2_900_000.0;
/// Projected warmup synthesis above this escalates the boot line from INFO to
/// WARN. No refusal: warmup length is the operator's call, and the obligation
/// this discharges is only that an extreme warmup fails loudly rather than
/// looking like a hung boot.
const WARMUP_WARN_SECS: f64 = 60.0;

/// The venue always binds loopback on an EPHEMERAL port, and there is no flag to
/// change either half.
///
/// The port is not choosable because one venue serves one run: a fixed default
/// port is what let two runs on one machine collide - or worse, silently connect
/// to each other's venue, with each other's config, clock and instrument - and a
/// per-run port an operator hand-assigns is the same problem with extra
/// bookkeeping. The kernel allocates, and the endpoint reaches its owner through
/// the readiness record (or, for a human, the `mogwai listening` log line).
///
/// The HOST is not choosable because the venue models latency on the sim axis
/// only and runs on the same machine as its client, where physical latency is
/// negligible by construction. Serving another interface would put a real
/// network under a fixture that does not model one.
const BIND_ADDR: &str = "127.0.0.1:0";

/// Arms the kernel's parent-death signal, and refuses to serve if the launcher
/// is already gone.
///
/// `PR_SET_PDEATHSIG` fires only on a FUTURE death of the parent, so on its own
/// it carries a race: a launcher that dies between forking this process and this
/// call is never signalled for, and the venue serves a run nobody owns, forever,
/// holding a port whose number died with the launcher that was going to read it.
/// With no PID file and no `stop` subcommand, the kernel is the only reaper
/// there is, so that window has to be closed here rather than tolerated.
///
/// Reading the parent BEFORE arming and again after closes it: if the two
/// differ, this process was reparented in between, which means the launcher died
/// inside the window and no signal is coming. Comparing against the observed
/// value rather than against pid 1 keeps this correct under a subreaper, where
/// an orphan is reparented to the subreaper and never to init.
///
/// Comparing samples cannot see a launcher that was ALREADY gone before the
/// first instruction ran: both reads return the reaper's pid, nothing changed,
/// and the venue serves a run nobody owns. That is not theoretical - a launcher
/// that spawns and exits immediately reproduces it every time. `expected` closes
/// it exactly: told the launcher's own pid, the venue can check it still has
/// that parent rather than infer from a change. The shipped launcher always
/// passes it.
///
/// A launcher that cannot pass it is not defenceless, and this is why the
/// contract says to CAPTURE stdout rather than merely suggesting it. The venue
/// writes its readiness line to a pipe whose read end died with the launcher, so
/// the write fails and the venue exits - later than this check, since it happens
/// after warmup, but before it has served anything. A launcher that both
/// declines the pid and lets the venue inherit its stdout gets neither guard and
/// will leave an orphan.
///
/// Comparing against the observed value rather than against pid 1 keeps all of
/// this correct under a subreaper, and refusing to special-case pid 1 keeps a
/// container's pid-1 launcher working, which a bare `getppid() == 1` test would
/// reject outright.
fn arm_parent_death_signal(expected: Option<i32>) -> anyhow::Result<()> {
    let before = nix::unistd::getppid();
    if let Some(expected) = expected
        && before != nix::unistd::Pid::from_raw(expected)
    {
        anyhow::bail!(
            "launcher {expected} is already gone (parent is {before}); refusing to serve a run \
             nobody owns"
        );
    }
    // SAFETY: prctl with PR_SET_PDEATHSIG takes a signal number by value and
    // touches no memory this process owns.
    unsafe {
        if nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGTERM) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    // A blocked SIGTERM is INHERITED ACROSS EXEC, and a blocked parent-death
    // signal is simply never delivered - the venue outlives its launcher with
    // the signal pending forever. That makes the whole cleanup story depend on
    // a property of whichever process happened to spawn us, which no launcher
    // documents and none can be asked to guarantee. Unblocking it here is the
    // only place that covers every launcher. Reproduced before this line
    // existed: a launcher that blocked SIGTERM, spawned the venue and exited
    // left it serving `/health` indefinitely.
    //
    // SIGTERM rather than SIGKILL stays deliberate - it feeds the graceful
    // drain in `shutdown_signal` - but that choice is only safe once delivery
    // is guaranteed.
    let mut just_sigterm = nix::sys::signal::SigSet::empty();
    just_sigterm.add(nix::sys::signal::Signal::SIGTERM);
    nix::sys::signal::sigprocmask(
        nix::sys::signal::SigmaskHow::SIG_UNBLOCK,
        Some(&just_sigterm),
        None,
    )
    .map_err(|err| anyhow::anyhow!("could not unblock SIGTERM: {err}"))?;
    let after = nix::unistd::getppid();
    if before != after {
        anyhow::bail!(
            "launcher died during startup (reparented from {before} to {after}); \
             refusing to serve a run nobody owns"
        );
    }
    Ok(())
}

/// Run one venue in the foreground until its declared duration elapses or a
/// signal arrives.
pub fn serve(
    config: Option<PathBuf>,
    duration: Option<Duration>,
    launcher_pid: Option<i32>,
) -> anyhow::Result<()> {
    arm_parent_death_signal(launcher_pid)?;
    init_stderr_logging()?;
    // The outer option records whether the CLI flag was present. The inner
    // option preserves its meaning: zero is an explicit indefinite override,
    // while a positive value is a declared completion.
    let duration_ns = duration.map(|duration| {
        let ns = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        (ns != 0).then_some(ns)
    });
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve_async(config, duration_ns))
}

async fn serve_async(
    config: Option<PathBuf>,
    duration_override_ns: Option<Option<u64>>,
) -> anyhow::Result<()> {
    let cfg = Config::load(config)?;
    let profiles = Arc::new(build_instrument_profiles(&cfg)?);
    let instrument = profiles.boot_symbol_def(cfg.boot_symbol())?;
    let seeds = mogwai_protocol::RunSeeds::from_run_seed(
        cfg.seed.unwrap_or_else(|| rand::random::<u64>() >> 1),
    );
    let run_start_ns = source::TAPE_ORIGIN_NS.saturating_add(cfg.warmup_ns);
    tracing::info!(
        run_seed = seeds.run,
        fill_seed = seeds.fill,
        "run seeds fixed"
    );
    let warm_symbol = Arc::clone(&instrument.symbol);
    let rivers = source::Rivers::new(
        source::TapeIdentity {
            seeds,
            regime: cfg.regime,
        },
        Arc::clone(&profiles),
    );
    // Boot projections. Both are advisory - warmup length and ring depth are
    // the operator's call - but an extreme warmup must fail LOUDLY rather than
    // look like a hung boot, and a ring sized for the old cadence is a
    // correctness problem (`FeedLagged` closes the socket with WS 1011), not a
    // tuning one. A missing profile is not fatal here: the run has already been
    // validated against the instrument set, so this only skips the advice.
    if let Some(profile) = profiles.configured(&instrument.symbol) {
        let projected_ticks =
            cfg.warmup_ns as f64 / 1_000_000_000.0 / profile.scalars.mean_event_duration_s
                * profile.scalars.children_mean;
        let projected_synthesis_s = projected_ticks / SYNTHESIS_TICKS_PER_SEC;
        if projected_synthesis_s > WARMUP_WARN_SECS {
            tracing::warn!(
                projected_ticks,
                projected_synthesis_s,
                "warmup synthesis is projected to exceed 60 seconds"
            );
        } else {
            tracing::info!(
                projected_ticks,
                projected_synthesis_s,
                "projected warmup synthesis cost"
            );
        }
        let projected_wall_frames =
            profile.scalars.children_mean / profile.scalars.mean_event_duration_s * cfg.speed;
        if (cfg.fanout_depth as f64) < projected_wall_frames {
            tracing::warn!(
                fanout_depth = cfg.fanout_depth,
                projected_wall_frames,
                "fanout ring holds less than one projected wall second"
            );
        }
    }
    let warm_rivers = Arc::clone(&rivers);
    let checkpoints =
        tokio::task::spawn_blocking(move || warm_rivers.ensure_reach(&warm_symbol, run_start_ns))
            .await??;
    let sim = build_run_clock(&cfg, now_ns())?;
    let data_origin_ns = source::TAPE_ORIGIN_NS;
    tracing::info!(
        checkpoints,
        warmup_ns = cfg.warmup_ns,
        run_start_ns,
        data_origin_ns,
        "eager warmup materialized"
    );
    let run_duration_ns = resolve_run_duration(duration_override_ns, cfg.run_duration_ns);
    // Construct this before Tape::start: a source can fault immediately on its
    // first walk, so a later shutdown channel would lose the terminal signal.
    let (fault_tx, fault_rx) = std::sync::mpsc::channel();
    let run = run::Run::new(
        instrument.clone(),
        Arc::clone(&rivers),
        cfg.balances.clone(),
        sim,
        run_start_ns,
        cfg.warmup_ns,
        run_duration_ns,
        seeds,
        cfg.fanout_depth,
        cfg.zero_speed_stall_ms,
        cfg.oms_type,
        cfg.fill_band_max_ticks,
        // Validated at load, so this cannot fail here.
        mogwai_protocol::AccountId::parse(cfg.account_id.trim())
            .map_err(|err| anyhow::anyhow!("account_id: {err}"))?,
        cfg.reset_account_on_reconnect,
        cfg.account_policies.clone(),
        fault_tx,
    );
    let boot_profile = rivers
        .resolve_profile(&instrument.symbol)
        .expect("validated boot profile");
    let boot_river = rivers.resolve_key(&boot_profile);
    let boot_ticket = run
        .boatyard
        .board(&crate::boatyard::BoardRequest {
            river: boot_river,
            speed: cfg.speed,
        })
        .await
        .map_err(|refusal| anyhow::anyhow!("could not place boot boat: {refusal:?}"))?;
    run.retain_boot_ticket(boot_ticket);
    tracing::info!(
        account_id = cfg.account_id.trim(),
        "run ledger account fixed"
    );
    tracing::info!(fill_seed = run.seeds.fill, "fill band stream initialized");
    // The band needs something to advance it: a submit decides only its own
    // order, against the reading it arrived with, so a resting limit fills only
    // when a sweep pass walks the tape it is waiting on. Spawned
    // unconditionally, because there is no configuration in which limits do not
    // rest.
    sweeper::spawn_fill_sweeper(sweeper::FillSweep {
        run: Arc::clone(&run),
        rivers: Arc::clone(&rivers),
        interval_ms: cfg.fill_sweep_interval_ms,
    });
    // The TTL reaper. A frozen account is resumable state with no lifecycle of
    // its own, so without this a long-lived shared exchange accumulates one
    // ledger per id anybody ever presented. Spawned only when a TTL is
    // configured: the default is to keep accounts forever, which is what a
    // consumer restarting a worker needs and what an operator who has not
    // thought about restart windows should get.
    if cfg.account_ttl_ms > 0 {
        let ttl = std::time::Duration::from_millis(cfg.account_ttl_ms);
        let reaping = Arc::clone(&run);
        // Swept at a fraction of the TTL, floored, so the effective lifetime
        // overshoots by at most that fraction rather than by a whole TTL.
        let cadence = (ttl / 10).max(std::time::Duration::from_millis(50));
        tokio::spawn(async move {
            let mut completion = reaping.completion();
            loop {
                tokio::select! {
                    () = tokio::time::sleep(cadence) => {}
                    _ = completion.changed() => break,
                }
                reaping.collect_expired_accounts(ttl);
            }
        });
    }
    let state = AppState {
        run,
        cfg: cfg.clone(),
        rivers: Arc::clone(&rivers),
        pending_commands: Arc::new(tokio::sync::Semaphore::new(cfg.global_pending_command_acts)),
        history_requests: Arc::new(tokio::sync::Semaphore::new(
            http::MAX_CONCURRENT_HISTORY_REQUESTS,
        )),
        history_queue: Arc::new(tokio::sync::Semaphore::new(
            http::MAX_QUEUED_HISTORY_REQUESTS,
        )),
    };
    let completing_run = Arc::clone(&state.run);
    // The run, kept past `with_state` below, for `serve_until_drained`'s
    // session wait.
    let sessions = Arc::clone(&state.run);
    let (fault_shutdown_tx, mut fault_shutdown_rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        if let Ok(fault) = fault_rx.recv() {
            let _shutdown_receiver_gone = fault_shutdown_tx.send(fault);
        }
    });
    let app = Router::new()
        .route("/health", get(http::health))
        .route("/account", get(account))
        .route("/instruments", get(instruments))
        .route("/trades", get(trades))
        .route("/quotes", get(quotes))
        .route("/clock", get(clock))
        .route("/ws", get(ws_upgrade))
        .route("/accounts", post(http::open_account))
        // HAVOC IS PER-ACCOUNT CONFIGURATION, POSTED ON CONNECT, AND CONSTANT
        // FOR THAT CONNECTION. An account's knobs arrive as it connects and do
        // not change again while it is connected; no consumer arms a divergence
        // against an account already trading, and no strategy ever sees its
        // knobs move underneath it.
        //
        // THE VENUE DOES NOT ENFORCE THIS, and the gap is stated rather than
        // implied: this route accepts an arm at any instant of a run, including
        // against an account mid-connection, and answers 202 for it. The
        // constraint is a convention held by the callers, so an operator who
        // POSTs late gets live mutation and the venue will not say otherwise.
        //
        // Recorded here because the endpoint is where someone looks before
        // arming something, and because several open items elsewhere - the
        // pending-arm shed cap, the unordered application of concurrent arms -
        // read as live hazards only under the assumption this convention does
        // not hold.
        .route("/control/divergence", post(arm_divergence))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(BIND_ADDR).await?;
    let bound_addr = listener.local_addr()?;
    // The readiness record goes to STDOUT, unconditionally, as the first and
    // only thing this process ever writes there. A launcher captures stdout and
    // reads one line; a human running by hand sees the same line in the
    // terminal. It is not gated behind a flag because the earlier `--ready-fd
    // <FD>` form took an unvalidated integer straight into `from_raw_fd`: a
    // number naming some OTHER inherited fd wrote the record into whatever that
    // was and then CLOSED it, while the launcher blocked forever on a pipe that
    // received neither a line nor an EOF. Stdout cannot be misaddressed, needs
    // no coordination between the spawn call and the argv, and is a first-class
    // primitive in every process API a launcher might be written against.
    //
    // Logs are on stderr (`init_stderr_logging`), so the two streams never
    // interleave and stdout stays exactly one line of JSON.
    let record = mogwai_protocol::ReadyRecord {
        version: mogwai_protocol::ReadyRecord::VERSION,
        addr: bound_addr,
        pid: std::process::id(),
        run_seed: seeds.run,
        data_origin_ns,
        run_start_ns,
        run_duration_ns,
        warmup_ns: cfg.warmup_ns,
        reset_account_on_reconnect: cfg.reset_account_on_reconnect,
        account_ttl_ms: cfg.account_ttl_ms,
        version_string: long_version(),
    };
    {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(&record)?)?;
        stdout.flush()?;
    }
    tracing::info!(%bound_addr, "mogwai listening");
    // The deadline task announces completion on every open socket and only
    // then stops the accept loop, so no connection is accepted after the
    // announcement it would never see. Without a declared duration there is no
    // deadline and the only way out is a signal.
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    // Without a deadline the sender is parked here rather than dropped: a
    // dropped sender makes `changed()` resolve immediately, which would shut an
    // indefinite run down the instant it started serving.
    // Set by the deadline task and read by `serve_until_drained`: a PLANNED
    // completion owes its announcement to every open socket, a signal does not.
    let planned_completion = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let deadline_completion = Arc::clone(&planned_completion);
    let _stop_tx_parked = match completing_run.deadline_ns {
        Some(deadline_ns) => {
            // SLEPT TO AN INSTANT, NOT FOR A SPAN. `sim` is anchored at
            // `build_run_clock(&cfg, now_ns())`, taken right after warmup, and
            // everything between that anchor and here - boat placement, the
            // listener bind, the readiness write - is wall time the run has
            // already spent. Sleeping `deadline_ns - started_ns` from HERE
            // measured the declared duration from the wrong epoch and overran
            // it by the whole boot interval, which at speed 100 is a large
            // multiple of the declared duration in sim terms.
            //
            // The remainder is already WALL nanoseconds - `wall_ns` did the
            // scaling - so it is slept raw rather than through `wall_duration`,
            // which would scale it a second time.
            //
            // SLEPT IN A LOOP UNTIL THE SIM CLOCK IS ACTUALLY PAST THE
            // DEADLINE, rather than once to a computed instant. `wall_ns`
            // truncates through the scaled conversion, so waking exactly at
            // `wall_ns(deadline_ns)` can leave `sim_ns(now)` a few hundred
            // nanoseconds SHORT of `deadline_ns` at a high speed - and the run
            // would then announce a completion carrying less elapsed sim time
            // than it declared. Nothing but the scheduler's own overshoot
            // covered that. The loop makes serving the whole declared duration
            // a property of this code instead - ON THE RUN CLOCK, which is the
            // only clock it can be a property of. A socket re-derives its
            // `RunComplete` on ITS BOAT's clock, and a boat is anchored at its
            // own placement, so the announcement one socket reads can still
            // carry slightly less elapsed sim time than the run declared. See
            // `reference/clock.md`; nothing here can close that, and the wait
            // that would - holding the run open until every boat's affine clock
            // has passed the deadline too - would let a socket connecting near
            // the deadline extend the run by another whole duration.
            //
            // The step is `deadline_wait`, so the never-break-early property is
            // testable without a host: see its tests.
            tokio::spawn(async move {
                while let Some(wait) = deadline_wait(sim, deadline_ns, now_ns()) {
                    tokio::time::sleep(wait).await;
                }
                let sim_now = sim.sim_ns(now_ns());
                completing_run.complete(sim_now, sim_now.saturating_sub(completing_run.started_ns));
                deadline_completion.store(true, std::sync::atomic::Ordering::Release);
                stop_tx.send(true).ok();
            });
            None
        }
        None => Some(stop_tx),
    };
    let terminal_fault = Arc::new(std::sync::Mutex::new(None));
    let terminal_fault_for_shutdown = Arc::clone(&terminal_fault);
    serve_until_drained(listener, app, sessions, planned_completion, async move {
        tokio::select! {
            // A signal means the launcher ended the run, not that its declared
            // simulated duration completed. Do not publish a false RunComplete.
            _ = shutdown_signal() => {},
            _ = stop_rx.changed() => {},
            received = &mut fault_shutdown_rx => {
                *terminal_fault_for_shutdown
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = received.ok();
            },
        }
    })
    .await?;
    if let Some(fault) = *terminal_fault
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        anyhow::bail!("tape source fault: {fault:?}");
    }
    Ok(())
}

/// How long the deadline task sleeps before asking again, or `None` once the
/// RUN CLOCK is genuinely past `deadline_ns`.
///
/// THE ONE RULE IS THAT `None` MEANS PAST, NEVER "CLOSE ENOUGH". `wall_ns`
/// truncates through the scaled conversion, so the wall instant it computes for
/// a deadline can map back to a sim instant a few hundred nanoseconds SHORT of
/// it at a high speed - and a task that woke there and stopped would announce a
/// completion carrying less elapsed sim time than the run declared, with only
/// the scheduler's own sleep overshoot standing between it and a false claim.
/// The remainder is already WALL nanoseconds, so it is returned raw rather than
/// through `wall_duration`, which would scale it a second time; a truncated
/// remainder can be zero while the sim clock is still short, so the wait floors
/// at a nanosecond and yields rather than spinning.
fn deadline_wait(
    sim: mogwai_protocol::SimClock,
    deadline_ns: u64,
    wall_now: u64,
) -> Option<Duration> {
    if sim.sim_ns(wall_now) >= deadline_ns {
        return None;
    }
    let remaining = sim.wall_ns(deadline_ns).saturating_sub(wall_now);
    Some(Duration::from_nanos(remaining.max(1)))
}

fn resolve_run_duration(override_ns: Option<Option<u64>>, configured_ns: u64) -> Option<u64> {
    override_ns.unwrap_or_else(|| (configured_ns != 0).then_some(configured_ns))
}

/// How long a completed or signalled venue waits for its live connections to
/// drain before exiting regardless. Wall time: this is a teardown deadline, not
/// a simulated one.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Serve until `trigger` fires, then let axum drain - but bound that drain by
/// `SHUTDOWN_GRACE`. Graceful shutdown waits for live connections, and a
/// websocket peer that never reads can hold one open indefinitely; an
/// unbounded wait would turn a completed or signalled venue into exactly the
/// orphan this lifecycle exists to remove.
///
/// AXUM'S DRAIN IS NOT THE WHOLE DRAIN, and that gap is what `run` and
/// `planned_completion` close. `axum::serve` waits on hyper CONNECTION futures,
/// and an upgraded connection's resolves at the 101 - so its serve future can
/// complete while every websocket session is still mid-frame. Returning there
/// drops the runtime and every session with it, which is how a planned
/// completion's `RunComplete` and its WS 1000 close went missing on a loaded
/// host. On a planned completion this therefore also waits for the sessions
/// themselves, still inside `SHUTDOWN_GRACE`; on a signal or a fault it does
/// not, because nothing has told those sessions to end and the wait would only
/// idle out the grace. See `Run::sessions_drained`.
async fn serve_until_drained(
    listener: tokio::net::TcpListener,
    app: Router,
    run: Arc<run::Run>,
    planned_completion: Arc<std::sync::atomic::AtomicBool>,
    trigger: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let (grace_tx, grace_rx) = tokio::sync::oneshot::channel();
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        trigger.await;
        grace_tx.send(()).ok();
    });
    let serve = std::future::IntoFuture::into_future(serve);
    tokio::pin!(serve);
    tokio::select! {
        result = async {
            let served = (&mut serve).await;
            if planned_completion.load(std::sync::atomic::Ordering::Acquire) {
                run.sessions_drained().await;
            }
            served
        } => result?,
        () = async move {
            // Only starts counting once the shutdown was actually triggered,
            // so this never bounds the run itself.
            grace_rx.await.ok();
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => anyhow::bail!(
            "connections did not drain within the {} ms shutdown grace",
            SHUTDOWN_GRACE.as_millis()
        ),
    }
    Ok(())
}

async fn shutdown_signal() {
    let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        return;
    };
    tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
}

fn init_stderr_logging() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mogwai=info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    /// The endpoint is not configurable, and this pins the two halves that
    /// matter: loopback, so no real network sits under a fixture that models
    /// none, and port zero, so the kernel allocates and two concurrent runs
    /// cannot collide on a shared default. A literal port reappearing here is
    /// the regression - it is what let one run connect to another's venue.
    #[test]
    fn the_venue_always_binds_an_ephemeral_loopback_port() {
        let addr: SocketAddr = BIND_ADDR.parse().expect("BIND_ADDR is a literal addr");
        assert!(addr.ip().is_loopback(), "{addr}");
        assert_eq!(addr.port(), 0, "{addr}");
    }

    /// THE DEADLINE TASK MAY NOT STOP EARLY, and the case that would is the
    /// wall instant its own conversion computes.
    ///
    /// Waking at `wall_ns(deadline)` and stopping there is the shape this
    /// replaced: at speed 100 the scaled conversion truncates, so that instant
    /// maps back SHORT of the deadline for most deadlines and the run would
    /// announce less elapsed sim time than it declared. The scan asserts the
    /// biconditional - a wait is owed exactly while the sim clock is short -
    /// and then asserts that the truncating case was actually reached, because
    /// a scan over deadlines that all convert exactly would prove nothing.
    #[test]
    fn the_deadline_wait_never_reports_done_before_the_sim_clock_arrives() {
        let sim = mogwai_protocol::SimClock {
            sim_epoch_ns: 300_000_000_000,
            wall_anchor_ns: 1_000_000_000,
            speed: 100.0,
        };
        let mut truncated = 0;
        for step in 0..1_000u64 {
            let deadline_ns = sim.sim_epoch_ns + 30_000_000_000 + step;
            let converted = sim.wall_ns(deadline_ns);
            let short = sim.sim_ns(converted) < deadline_ns;
            assert_eq!(
                deadline_wait(sim, deadline_ns, converted).is_some(),
                short,
                "a wait is owed exactly while the sim clock is short of {deadline_ns}"
            );
            if short {
                truncated += 1;
            }
            // And once the sim clock IS past it, the task stops rather than
            // sleeping the run out one nanosecond at a time.
            assert!(
                deadline_wait(sim, deadline_ns, converted + 1_000_000).is_none(),
                "the wait ends once the deadline is genuinely behind us"
            );
        }
        assert!(
            truncated > 0,
            "the conversion never truncated, so this scan proved nothing"
        );
    }

    #[test]
    fn an_explicit_zero_duration_overrides_a_finite_config() {
        assert_eq!(resolve_run_duration(Some(None), 600_000_000_000), None);
        assert_eq!(
            resolve_run_duration(None, 600_000_000_000),
            Some(600_000_000_000)
        );
    }
}
