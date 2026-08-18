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
mod characterize;
mod r#gen;
mod man;
mod preflight;
mod segments;
mod select_windows;
mod session_profile;
mod synth;
mod tick_composition;
mod tick_composition_ratios;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use mogwai_cli::{
    arrival_control, arrival_envelope_diagnostic, arrival_screen, count_curve, fit, measure,
    minute_range_envelope, stage_m,
};
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
    /// The signed count-curve preregistration's generated-only Stage 0 backcheck.
    CountCurve(count_curve::CountCurveArgs),
    /// Stage M Tier 1 per-month measurement, July backcheck and pooled test.
    StageM(stage_m::StageMArgs),
    /// The protocol-11 session calibration fit: the observed corpus pass,
    /// the closed-form session refits, the CRN vol_scalar solve and the
    /// family probes, written as the hash-bound fit artifact.
    Fit(fit::FitArgs),
    /// Build Brick B4's bound minute-range-envelope artifact.
    MinuteRangeEnvelope(minute_range_envelope::MinuteRangeEnvelopeArgs),
    /// Protocol 12b brick N: the deterministic hourly re-centring negative control.
    ArrivalControl(arrival_control::ArrivalControlArgs),
    /// Protocol 12b Stage A necessary-condition screen.
    ArrivalScreen(arrival_screen::ArrivalScreenArgs),
    /// Evaluate skipped coarse A2 envelopes without changing the official screen.
    ArrivalEnvelopeDiagnostic(arrival_envelope_diagnostic::ArrivalEnvelopeDiagnosticArgs),
    /// The cache-storage-class manual controls (stats / clean / clean
    /// --stale).
    Cache(cache::CacheArgs),
    /// Stream a trade corpus into `char_<PAIR>.json` stylized-fact reports,
    /// the input `synth fingerprint` reads. Covers both the per-corpus and
    /// the multi-pair fan-out cases.
    Characterize(characterize::CharacterizeArgs),
    /// Bar-frame intake: choose which tick-data windows to buy, stratifying
    /// on what a one-minute bar can measure.
    SelectWindows(select_windows::SelectWindowsArgs),
    /// Apply the BBO budget sizing policy to composition fixtures, proposing
    /// the four sized constants.
    TickCompositionRatios(tick_composition_ratios::RatiosArgs),
    /// The calendar-conditional session fit over a one-minute bar archive.
    SessionProfile {
        #[command(subcommand)]
        command: session_profile::SessionProfileCommand,
    },
    /// Offline synthesis: fingerprint and cadence generation.
    Synth {
        #[command(subcommand)]
        command: synth::SynthCommand,
    },
    /// The L0 structural-proceed verdict over a cadence measurement, plus the
    /// Markov density feasibility gate.
    CadenceFeasible(synth::CadenceFeasibleArgs),
    /// The session-segment sampler: cut real session slices out of a delivered
    /// TBBO month, and compose them into an endless single-session tape.
    Segments {
        #[command(subcommand)]
        command: segments::SegmentsCommand,
    },
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
    // FIRST, before the argv parse: the benchmark marker timeline is measured
    // from here, so anything ahead of this line is invisible to a phase
    // decomposition. A no-op outside a benchmark harness.
    mogwai_lab::sidecar::init();
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
                for name in config::preset_names() {
                    println!("{name}");
                }
            }
            Ok(())
        }
        Command::Man(args) => {
            man::run(args.topic);
            Ok(())
        }
        Command::Preflight(args) => preflight::run(args),
        Command::Measure(args) => measure::run(args),
        Command::CountCurve(args) => count_curve::run(&args),
        Command::StageM(args) => stage_m::run(args),
        Command::Fit(args) => fit::run(&args),
        Command::MinuteRangeEnvelope(args) => {
            minute_range_envelope::run(args)?;
            Ok(())
        }
        Command::ArrivalControl(args) => {
            arrival_control::run(args)?;
            Ok(())
        }
        Command::ArrivalScreen(args) => {
            arrival_screen::run(args)?;
            Ok(())
        }
        Command::ArrivalEnvelopeDiagnostic(args) => {
            arrival_envelope_diagnostic::run(args)?;
            Ok(())
        }
        Command::Cache(args) => cache::run(args),
        Command::Characterize(args) => characterize::run(args),
        Command::SelectWindows(args) => select_windows::run(&args),
        Command::TickCompositionRatios(args) => tick_composition_ratios::run(&args),
        Command::SessionProfile { command } => session_profile::run(command),
        Command::Synth { command } => synth::run(command),
        Command::CadenceFeasible(args) => synth::run_cadence_feasible(args),
        Command::Segments { command } => segments::run(command),
    }
}
