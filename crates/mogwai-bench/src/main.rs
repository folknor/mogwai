// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! mogwai's benchmark instruments, one subcommand each, kept out of the
//! shipped `mogwai` binary. The script benches drive them; each reports on
//! stderr as `key=value` lines for its script to parse.
//!
//! The lifecycle: the hotpath guard starts only when the runner asks for a
//! report (`HOTPATH_OUTPUT_PATH` set), the arguments are parsed with `try_parse` so
//! clap's own exit never skips the guard, the instrument runs, and the guard
//! drops on the ordinary return path, writing its report. Every instrument
//! returns rather than calling `std::process::exit` for the same reason; a
//! panic unwinds through `main` and drops the guard too.
//!
//! One binary has one global allocator, and three want the slot: the system
//! allocator, hotpath's counting allocator under `hotpath-alloc`, and
//! `tag-decode`'s own counter. Resolved by feature:
//!
//! - `hotpath-alloc`: `hotpath::CountingAllocator`.
//! - `hotpath` without alloc: none declared, so `System`, as the old example
//!   harnesses had.
//! - neither: `tag_decode::Switch`, which forwards straight to `System` until
//!   `tag-decode` arms it, so every other instrument pays one relaxed load per
//!   allocator call and nothing else.
//!
//! `tag-decode` refuses to run in either instrumented build: under
//! `hotpath-alloc` its counter is not installed, and under `hotpath` a live
//! guard's background thread would allocate into the process-wide count.

mod arrival_walk;
mod envelope_evaluation;
mod order_path_refold;
mod ring_sizing;
mod screen_projection;
mod stage_a_batch;
mod tag_decode;
mod tape_lateness;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// hotpath does not install its counting allocator itself - the crate exposes
/// it and leaves the choice to the binary. Without this an alloc-mode run
/// completes, reports zero bytes for every frame, and looks like a program
/// that does not allocate.
#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOC: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

#[cfg(not(feature = "hotpath"))]
#[global_allocator]
static ALLOC: tag_decode::Switch = tag_decode::Switch;

#[derive(Parser)]
#[command(name = "mogwai-bench", about = "mogwai benchmark instruments")]
struct Cli {
    #[command(subcommand)]
    instrument: Instrument,
}

#[derive(Subcommand)]
enum Instrument {
    /// The parent draw with no projection attached
    ArrivalWalk,
    /// One Stage A screen cell per family, profiled
    ScreenProjection,
    /// How late a paced tape arrives at a consumer that keeps up
    TapeLateness {
        /// Venue config; empty or absent means the accelerated fixture
        config: Option<String>,
        /// Sample length in seconds
        #[arg(default_value_t = 10.0)]
        sample_seconds: f64,
    },
    /// The eager resident cost of one shipped per-boat ring
    RingSizing,
    /// Reproducible quick and full panels for Stage A throughput work
    #[command(name = "stage-a-batch", args_override_self = true)]
    StageABatch(stage_a_batch::Args),
    /// Predictive-envelope replicate months, priced
    EnvelopeEvaluation {
        /// Arrival family, in the seam spelling
        #[arg(value_enum, default_value = "shot_noise")]
        family: envelope_evaluation::FamilyArg,
        /// Replicate months to simulate
        #[arg(default_value_t = 32)]
        months: usize,
    },
    /// Per-frame cost of the wire decoder over one `Trade`
    TagDecode {
        /// Iterations per arm
        #[arg(default_value_t = 2_000_000)]
        iterations: usize,
    },
    /// Submit, amend and cancel against the aggregate hold fold
    OrderPathRefold {
        /// Resting orders on the book
        #[arg(default_value_t = 50)]
        resting: usize,
        /// Submit-amend-cancel rounds
        #[arg(default_value_t = 2_000)]
        rounds: usize,
    },
}

fn main() -> ExitCode {
    // Only when the runner asks for a report: without the request an instrumented
    // build would start hotpath's metrics server. Bound to a named guard,
    // because the report is written by its Drop impl.
    #[cfg(feature = "hotpath")]
    let _guard = std::env::var_os("HOTPATH_OUTPUT_PATH")
        .is_some()
        .then(|| hotpath::HotpathGuardBuilder::new("main").build());

    // `try_parse`, never `parse`: clap's own exit would skip the guard.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            drop(e.print());
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };

    match cli.instrument {
        Instrument::ArrivalWalk => arrival_walk::run(),
        Instrument::ScreenProjection => screen_projection::run(),
        Instrument::TapeLateness {
            config,
            sample_seconds,
        } => tape_lateness::run(config, sample_seconds),
        Instrument::RingSizing => ring_sizing::run(),
        Instrument::StageABatch(args) => return stage_a_batch::run(&args),
        Instrument::EnvelopeEvaluation { family, months } => {
            envelope_evaluation::run(family, months);
        }
        Instrument::TagDecode { iterations } => return tag_decode::run(iterations),
        Instrument::OrderPathRefold { resting, rounds } => order_path_refold::run(resting, rounds),
    }
    ExitCode::SUCCESS
    // `_guard` drops here, writing the hotpath report.
}
