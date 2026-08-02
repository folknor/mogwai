// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! One foreground mogwai venue process.

#[cfg(not(unix))]
compile_error!("mogwai-server requires a Unix target");

mod admission;
mod config;
mod fills;
mod r#gen;
mod http;
mod run;
mod source;
mod sweeper;
mod tape;
mod ws;

/// The fill-timing distribution certification. Test-only, and here rather than
/// in `tests/` because this is the only place that can see
/// `fills::scan_triggers`, `Engine::apply_scans` and `InstrumentProfiles`
/// at once - precisely the seam it certifies.
#[cfg(test)]
mod fill_golden;

use std::{
    fs::File,
    io::Write,
    net::SocketAddr,
    os::fd::{FromRawFd, RawFd},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    routing::{get, post},
};
use clap::{Args, Parser, Subcommand};

use crate::{
    config::{Config, build_instrument_profiles, build_run_clock, now_ns, refuse_unfunded_quotes},
    http::{AppState, account, arm_divergence, clock, instruments, quotes, trades},
    ws::ws_upgrade,
};

const LONG_VERSION: &str = env!("MOGWAI_LONG_VERSION");

/// Raw fills the generator synthesizes per wall second, used only to project
/// warmup cost at boot. It is a MEASURED number, not a target - re-read it off
/// the `fill_bench` row in `reference/performance.md` whenever the walk changes,
/// or the projection quietly drifts away from what boot actually costs.
const SYNTHESIS_TICKS_PER_SEC: f64 = 5_000_000.0;
/// Projected warmup synthesis above this escalates the boot line from INFO to
/// WARN. No refusal: warmup length is the operator's call, and the obligation
/// this discharges is only that an extreme warmup fails loudly rather than
/// looking like a hung boot.
const WARMUP_WARN_SECS: f64 = 60.0;

fn long_version() -> String {
    format!("{LONG_VERSION} tape {}", mogwai_data::TAPE_PROTOCOL_VERSION)
}

#[derive(Parser)]
#[command(name = "mogwai", version = long_version(), about = "Fake broker/exchange that drives broadarrow's live trading path", arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve(ServeArgs),
    Gen(r#gen::GenArgs),
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:0")]
    addr: SocketAddr,
    #[arg(long, value_name = "FD")]
    ready_fd: Option<RawFd>,
    #[arg(long, value_name = "DURATION")]
    duration: Option<humantime::Duration>,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Serve(args) => serve(args),
        Command::Gen(args) => r#gen::run(args),
    }
}

/// Decision 9: an ephemeral port reported nowhere serves nobody. Any explicit
/// non-zero port without a ready fd is fine - the caller already knows the
/// endpoint - so only the combination is refused, and the message names both
/// flags.
fn check_endpoint_is_discoverable(addr: SocketAddr, ready_fd: Option<RawFd>) -> anyhow::Result<()> {
    if addr.port() == 0 && ready_fd.is_none() {
        anyhow::bail!(
            "--addr with port 0 requires --ready-fd; otherwise the endpoint cannot be discovered"
        );
    }
    Ok(())
}

fn serve(args: ServeArgs) -> anyhow::Result<()> {
    check_endpoint_is_discoverable(args.addr, args.ready_fd)?;
    unsafe {
        if nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGTERM) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    init_stderr_logging()?;
    let duration_ns = args
        .duration
        .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX));
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve_async(
            args.config,
            args.addr,
            args.ready_fd,
            duration_ns,
        ))
}

async fn serve_async(
    config: Option<PathBuf>,
    addr: SocketAddr,
    ready_fd: Option<RawFd>,
    duration_override_ns: Option<u64>,
) -> anyhow::Result<()> {
    let cfg = Config::load(config)?;
    let profiles = Arc::new(build_instrument_profiles(&cfg)?);
    refuse_unfunded_quotes(&cfg, &profiles.instrument_defs())?;
    let instrument = profiles
        .instrument_defs()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no instrument configured"))?;

    let seeds = mogwai_protocol::RunSeeds::from_run_seed(
        cfg.seed.unwrap_or_else(|| rand::random::<u64>() >> 1),
    );
    let run_start_ns = source::TAPE_ORIGIN_NS.saturating_add(cfg.warmup_ns);
    tracing::info!(
        run_seed = seeds.run,
        tape_seed = seeds.tape,
        fill_seed = seeds.fill,
        "run seeds fixed"
    );
    let warm_symbol = instrument.symbol.clone();
    let warm_profiles = Arc::clone(&profiles);
    let warm_boot = source::BootTape {
        seeds,
        regime: cfg.regime,
    };
    // Boot projections. Both are advisory - warmup length and ring depth are
    // the operator's call - but an extreme warmup must fail LOUDLY rather than
    // look like a hung boot, and a ring sized for the old cadence is a
    // correctness problem (`FeedLagged` closes the socket with WS 1011), not a
    // tuning one. A missing profile is not fatal here: the run has already been
    // validated against the instrument set, so this only skips the advice.
    if let Some(profile) = profiles.get(&instrument.symbol) {
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
    let checkpoints = tokio::task::spawn_blocking(move || {
        source::materialize_warmup(&warm_symbol, &warm_profiles, warm_boot, run_start_ns)
    })
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
    let run_duration_ns =
        duration_override_ns.or_else(|| (cfg.run_duration_ns != 0).then_some(cfg.run_duration_ns));
    let run = run::Run::new(
        instrument.clone(),
        Arc::clone(&profiles),
        cfg.balances.clone(),
        sim,
        run_start_ns,
        cfg.warmup_ns,
        run_duration_ns,
        seeds,
        cfg.speed,
        cfg.fanout_depth,
        cfg.zero_speed_stall_ms,
    );
    tracing::info!(fill_seed = run.seeds.fill, "fill band stream initialized");
    // The band needs something to advance it: a submit decides only its own
    // order, against the reading it arrived with, so a resting limit fills only
    // when a sweep pass walks the tape it is waiting on. Spawned
    // unconditionally, because there is no configuration in which limits do not
    // rest.
    sweeper::spawn_fill_sweeper(sweeper::FillSweep {
        run: Arc::clone(&run),
        profiles: Arc::clone(&profiles),
        interval_ms: cfg.fill_sweep_interval_ms,
    });
    let state = AppState {
        run,
        cfg: cfg.clone(),
        profiles: Arc::clone(&profiles),
        pending_acts: Arc::new(tokio::sync::Semaphore::new(cfg.global_pending_command_acts)),
        market_readings: Arc::new(fills::MarketReadingCache::default()),
    };
    let completing_run = Arc::clone(&state.run);
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/account", get(account))
        .route("/instruments", get(instruments))
        .route("/trades", get(trades))
        .route("/quotes", get(quotes))
        .route("/clock", get(clock))
        .route("/ws", get(ws_upgrade))
        .route("/control/divergence", post(arm_divergence))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;
    if let Some(fd) = ready_fd {
        let symbol = instrument.symbol.clone();
        let record = mogwai_protocol::ReadyRecord {
            version: mogwai_protocol::ReadyRecord::VERSION,
            addr: bound_addr,
            pid: std::process::id(),
            symbol: symbol.clone(),
            run_seed: seeds.run,
            data_origin_ns,
            run_start_ns,
            run_duration_ns,
            warmup_ns: cfg.warmup_ns,
            version_string: long_version(),
        };
        let mut ready = unsafe { File::from_raw_fd(fd) };
        ready.write_all(format!("{}\n", serde_json::to_string(&record)?).as_bytes())?;
        ready.flush()?;
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
    let _stop_tx_parked = match completing_run.deadline_ns {
        Some(deadline_ns) => {
            let remaining_ns = deadline_ns.saturating_sub(completing_run.started_ns);
            tokio::spawn(async move {
                tokio::time::sleep(sim.wall_duration(remaining_ns)).await;
                let sim_now = sim.sim_ns(now_ns());
                completing_run.complete(sim_now, sim_now.saturating_sub(completing_run.started_ns));
                stop_tx.send(true).ok();
            });
            None
        }
        None => Some(stop_tx),
    };
    serve_until_drained(listener, app, async move {
        tokio::select! { _ = shutdown_signal() => {}, _ = stop_rx.changed() => {} }
    })
    .await
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
async fn serve_until_drained(
    listener: tokio::net::TcpListener,
    app: Router,
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
        result = &mut serve => result?,
        () = async move {
            // Only starts counting once the shutdown was actually triggered,
            // so this never bounds the run itself.
            grace_rx.await.ok();
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => tracing::warn!(grace_ms = SHUTDOWN_GRACE.as_millis(), "connections did not drain within the shutdown grace; exiting anyway"),
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

    #[test]
    fn serve_refuses_ephemeral_port_without_a_ready_fd() {
        let ephemeral: SocketAddr = "127.0.0.1:0".parse().expect("literal addr");
        let err = check_endpoint_is_discoverable(ephemeral, None)
            .expect_err("a port reported nowhere serves nobody");
        let message = err.to_string();
        assert!(message.contains("--addr"), "{message}");
        assert!(message.contains("--ready-fd"), "{message}");

        check_endpoint_is_discoverable(ephemeral, Some(3))
            .expect("an ephemeral port IS discoverable once it is reported");
        check_endpoint_is_discoverable("127.0.0.1:8787".parse().expect("literal addr"), None)
            .expect("an explicit port needs no report; the caller already knows it");
    }
}
