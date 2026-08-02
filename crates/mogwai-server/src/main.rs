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
/// `fills::count_penetrations`, `Engine::apply_scans` and `InstrumentProfiles`
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
    config::{Config, build_instrument_profiles, build_sim_clock, now_ns, refuse_unfunded_quotes},
    http::{AppState, account, arm_divergence, clock, instruments, quotes, trades},
    ws::ws_upgrade,
};

const LONG_VERSION: &str = env!("MOGWAI_LONG_VERSION");

#[derive(Parser)]
#[command(name = "mogwai", version = LONG_VERSION, about = "Fake broker/exchange that drives broadarrow's live trading path", arg_required_else_help = true)]
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

    // Decision 8: sim time must not advance across warmup generation, so the
    // clock is anchored AFTER the warmup is materialized rather than before.
    // The pre-warm clock exists only to name the window to generate; the run's
    // own clock, and therefore `run_start_ns`, is built once that work is paid
    // for. A scaled clock re-anchors onto the same sim epoch, so the two
    // windows coincide exactly; the identity clock cannot be re-anchored and
    // slides by the wall cost of warmup, which is why `data_origin_ns` is
    // re-derived from the FINAL `run_start_ns` below and not from the estimate.
    let prewarm = build_sim_clock(&cfg, now_ns())?;
    let prewarm_start_ns = prewarm.sim_ns(now_ns());
    let prewarm_origin_ns = prewarm_start_ns.saturating_sub(cfg.warmup_ns);
    let warm_symbol = instrument.symbol.clone();
    let warm_profiles = Arc::clone(&profiles);
    let warm_regime = cfg.regime;
    let checkpoints = tokio::task::spawn_blocking(move || {
        source::materialize_warmup(
            &warm_symbol,
            &warm_profiles,
            warm_regime,
            prewarm_origin_ns,
            prewarm_start_ns,
        )
    })
    .await??;
    let sim = build_sim_clock(&cfg, now_ns())?;
    let run_start_ns = sim.sim_ns(now_ns());
    let data_origin_ns = run_start_ns.saturating_sub(cfg.warmup_ns);
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
        cfg.penetration_ticks,
        sim,
        run_start_ns,
        cfg.warmup_ns,
        run_duration_ns,
        cfg.speed,
        cfg.gap_cap_ms,
        cfg.fanout_depth,
        cfg.zero_speed_stall_ms,
    );
    // The penetration gate needs something to advance it: a submit seeds only
    // its own order, so a resting limit fills only when a sweep pass walks the
    // tape it is waiting on. Spawned only when the gate is on, so a default
    // venue pays nothing for it.
    if cfg.penetration_ticks > 0 {
        sweeper::spawn_fill_sweeper(sweeper::FillSweep {
            run: Arc::clone(&run),
            profiles: Arc::clone(&profiles),
            interval_ms: cfg.fill_sweep_interval_ms,
        });
    }
    let state = AppState {
        run,
        cfg: cfg.clone(),
        profiles: Arc::clone(&profiles),
        pending_acts: Arc::new(tokio::sync::Semaphore::new(cfg.global_pending_command_acts)),
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
            seed: mogwai_protocol::SeedReport::PerSymbolFnv(vec![(
                symbol.clone(),
                source::seed_for(&symbol),
            )]),
            data_origin_ns,
            run_start_ns,
            run_duration_ns,
            warmup_ns: cfg.warmup_ns,
            version_string: LONG_VERSION.into(),
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
