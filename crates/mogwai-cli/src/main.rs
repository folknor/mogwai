// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The `mogwai` command: the clap dispatcher over the venue venue and the
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
#[cfg(test)]
mod test_paths;
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
    /// KEEP THIS ON `humantime`. The shipped launcher renders `--duration`
    /// with `mogwai-protocol`'s `format_duration`, which emits `0s`, `ms` and
    /// `ns`; `gen`'s in-house `parse_duration` reads none of the three. See
    /// `serve_argv_parses_in_the_venues_own_grammar` below.
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

/// A preset document as `mogwai presets <name>` prints it: the document with
/// exactly one trailing newline, whatever the bundled file carries.
///
/// The dispatcher used a bare `print!` of the document, so the trailing newline
/// was a property of the INCLUDED FILE rather than of the command. A preset
/// saved without a final newline left the shell prompt mid-line; one saved with
/// a blank line at the end printed two. Every shipped preset happens to end in
/// exactly one newline today, so nothing in this workspace can currently
/// observe the difference - which is why this is a function with its own unit
/// tests rather than a one-liner at the call site.
fn preset_output(document: &str) -> String {
    format!("{}\n", document.trim_end_matches('\n'))
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
                print!("{}", preset_output(document));
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::Parser;
    use mogwai_protocol::launch::{LaunchSpec, serve_argv};

    use super::{Cli, Command, r#gen, preset_output};

    /// The three endings a preset FILE can have, normalized to the one ending
    /// the terminal needs.
    ///
    /// The middle case is the one the shipped presets exercise and is
    /// therefore the only one the end-to-end test in `presets_cli.rs` can see;
    /// the other two are the ones the old `print!` got wrong and no committed
    /// preset can produce. Asserting the empty document too pins that the
    /// normalization does not invent a document out of nothing - it emits the
    /// bare newline, not a blank line plus one.
    #[test]
    fn a_preset_document_prints_with_exactly_one_trailing_newline() {
        assert_eq!(
            preset_output("symbol = \"MNQ\""),
            "symbol = \"MNQ\"\n",
            "a document with no trailing newline must not leave the prompt mid-line"
        );
        assert_eq!(
            preset_output("symbol = \"MNQ\"\n"),
            "symbol = \"MNQ\"\n",
            "a document already ending in one newline must pass through unchanged"
        );
        assert_eq!(
            preset_output("symbol = \"MNQ\"\n\n\n"),
            "symbol = \"MNQ\"\n",
            "trailing blank lines must not reach the terminal"
        );
        assert_eq!(
            preset_output(""),
            "\n",
            "an empty document prints one newline, not a blank line plus one"
        );
    }

    /// THE LAUNCHER'S RENDERING AND THE VENUE'S PARSER ARE ONE CONTRACT, AND
    /// THIS IS THE ONLY PLACE THE TWO MEET.
    ///
    /// `mogwai-protocol`'s `format_duration` promises that "every value renders
    /// into something the venue accepts", and until this test that promise was
    /// asserted only against itself - a golden-string test in the same module,
    /// with nothing on the parsing side. It matters because this binary carries
    /// TWO duration grammars that disagree on exactly the units the launcher
    /// emits: `serve` takes `humantime::Duration`, which accepts `ms`, `ns` and
    /// `0s`, while `gen`'s in-house `parse_duration` accepts only
    /// `s m h d w mo y`, refuses a zero count, and reads `1500ms` as an unknown
    /// unit (it has `mo`, not `ms`). Switching `serve` to the in-house parser
    /// "for consistency" would silently break `Duration::ZERO` and every
    /// millisecond and nanosecond value the launcher can produce, and nothing
    /// else would notice.
    ///
    /// It is a real crossing rather than the two-copy shape - a round trip
    /// through one crate's own pair of functions - because the rendering side
    /// is `mogwai-protocol`'s shipped `serve_argv` and the parsing side is the
    /// `Cli` this binary's `main` actually runs. It lives in the bin crate's
    /// unit tests because `ServeArgs` is private to it.
    ///
    /// THE CASES ARE THE ONES `durations_render_in_the_coarsest_exact_unit`
    /// ENUMERATES, so the two tests cannot drift onto different values: zero,
    /// a whole-second span, a millisecond remainder, one nanosecond, and a
    /// microsecond that renders as nanoseconds.
    #[test]
    fn serve_argv_parses_in_the_venues_own_grammar() {
        for duration in [
            Duration::ZERO,
            Duration::from_secs(90),
            Duration::from_millis(1_500),
            Duration::from_nanos(1),
            Duration::from_micros(1),
        ] {
            let spec = LaunchSpec {
                duration: Some(duration),
                ..LaunchSpec::default()
            };
            let mut argv = vec![std::ffi::OsString::from("mogwai")];
            argv.extend(serve_argv(&spec, std::process::id()));
            let rendered = format!("{argv:?}");
            let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| {
                panic!("the venue must parse its own launcher's argv {rendered}: {e}")
            });
            let Command::Serve(args) = cli.command else {
                panic!("serve_argv must render the serve subcommand, got {rendered}");
            };
            let parsed: Duration = args
                .duration
                .unwrap_or_else(|| panic!("--duration must survive the round trip: {rendered}"))
                .into();
            assert_eq!(
                parsed, duration,
                "the venue must read back the duration the launcher rendered, from {rendered}"
            );
        }
    }

    /// The companion refusal, so the test above cannot be read as "any parser
    /// would do". `gen`'s grammar is the one a later unification would reach
    /// for, and these are the renderings it cannot read.
    #[test]
    fn the_in_house_gen_grammar_cannot_read_what_the_launcher_renders() {
        for duration in [
            Duration::ZERO,
            Duration::from_millis(1_500),
            Duration::from_nanos(1),
        ] {
            let spec = LaunchSpec {
                duration: Some(duration),
                ..LaunchSpec::default()
            };
            let argv = serve_argv(&spec, std::process::id());
            let rendered = argv
                .last()
                .expect("--duration is last")
                .to_str()
                .expect("ascii")
                .to_owned();
            assert!(
                r#gen::parse_duration(&rendered).is_err(),
                "gen's grammar must not be assumed compatible: it read {rendered}"
            );
        }
    }
}
