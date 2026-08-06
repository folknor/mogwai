// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The `mogwai` command: the clap dispatcher over the venue server and the
//! offline generator/measurement subcommands.
//!
//! `serve` is the only subcommand that runs a venue, and it does no work here -
//! it hands its three arguments to `mogwai_server::serve`. Everything else in
//! this crate is offline tooling that never binds a socket.

#[cfg(not(unix))]
compile_error!("mogwai requires a Unix target");

mod cache;
mod r#gen;
mod man;
mod measure12a;
mod preflight;
mod tick_composition;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use mogwai_cli::measure;
use mogwai_server::{config, long_version};

#[derive(Parser)]
#[command(name = "mogwai", version = long_version(), about = "Fake broker/exchange that drives a nautilus live trading path", arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve(ServeArgs),
    // Boxed: GenArgs grew past clippy's variant-size threshold when the
    // trace window landed, and the CLI parses exactly once.
    Gen(Box<r#gen::GenArgs>),
    Presets(PresetArgs),
    /// Measure BBO protocol composition and write the paired budget fixtures.
    TickComposition(tick_composition::TickCompositionArgs),
    /// Print a bundled reference doc, or list the topics.
    Man(ManArgs),
    /// Fail-closed TBBO corpus contract check; persists a hash-bound
    /// preflight artifact.
    Preflight(preflight::PreflightArgs),
    /// The protocol-12a section-10 measurement gate: the live observed pass
    /// plus the eight in-process attestation walks, assembled and
    /// validated into the committed artifact shape.
    Measure(measure::MeasureArgs),
    /// The cache-storage-class manual controls (stats / clean / clean
    /// --stale).
    Cache(cache::CacheArgs),
}

#[derive(Args)]
struct ManArgs {
    /// Reference topic to display. Omit to list the available topics.
    #[arg(value_name = "TOPIC")]
    topic: Option<man::ManTopic>,
}

#[derive(Args)]
struct PresetArgs {
    name: Option<String>,
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(long, value_name = "DURATION")]
    duration: Option<humantime::Duration>,
    /// The launcher's own pid, so the venue can prove it still has the owner it
    /// was started by.
    ///
    /// Optional, and the shipped launcher always passes it. Without it the venue
    /// can only notice a launcher that dies DURING its startup, never one
    /// already gone before the first instruction ran - see
    /// `arm_parent_death_signal`.
    #[arg(long, value_name = "PID")]
    launcher_pid: Option<i32>,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Serve(args) => mogwai_server::serve(
            args.config,
            args.duration.map(std::convert::Into::into),
            args.launcher_pid,
        ),
        Command::Gen(args) => r#gen::run(*args),
        Command::TickComposition(args) => tick_composition::run(&args),
        Command::Presets(args) => {
            if let Some(name) = args.name {
                let document = config::preset_document(&name)
                    .ok_or_else(|| anyhow::anyhow!("unknown preset {name}"))?;
                print!("{document}");
            } else {
                println!("MNQ\nMES\nBTCUSDT\nETHUSDT\nSOLUSDT");
            }
            Ok(())
        }
        Command::Man(args) => {
            man::run(args.topic);
            Ok(())
        }
        Command::Preflight(args) => preflight::run(args),
        Command::Measure(args) => measure::run(args),
        Command::Cache(args) => cache::run(args),
    }
}
