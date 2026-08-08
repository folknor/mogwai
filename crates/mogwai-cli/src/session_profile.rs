// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai session-profile`: the calendar-conditional session fit over a
//! one-minute bar archive (`analysis/fit_session_profile.py`).
//!
//! The second CLI gap the 2026-08-08 program review found. `mogwai-lab`
//! carried the fit as a library, but nothing exposed it, so retiring the
//! Python would have removed a runnable fitting capability from the shipped
//! tool - the same class as the `characterize` gap, one level along.
//!
//! `preflight` is a GATE rather than a summary: it answers whether the
//! archive carries zero-volume rows, which decides whether exposure may come
//! from row presence or must come from the calendar. Run it before trusting
//! any fit. Deriving exposure from row presence would shrink each quiet
//! hour's denominator in proportion to its own quietness and compress the
//! peak-to-trough ratio the whole fit exists to measure.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use mogwai_lab::session_profile::Alignment;

#[derive(Subcommand)]
pub(crate) enum SessionProfileCommand {
    /// What the archive actually contains: row counts, session eligibility,
    /// early closes, and the zero-volume-row question the estimator turns on.
    Preflight(SessionProfileArgs),
    /// The multiplicative Poisson fit with an exposure offset, per scope,
    /// plus its separability and era-stability verdicts.
    Fit(SessionProfileArgs),
}

#[derive(Args)]
pub(crate) struct SessionProfileArgs {
    /// The one-minute bar archive. Defaults to the NQ continuous zip.
    #[arg(long, value_name = "PATH")]
    archive: Option<PathBuf>,
    /// The preset supplying the session calendar the fit is conditional on.
    /// The calendar is an INPUT to the estimator rather than a consumer of
    /// it, so this decides what the fit means.
    #[arg(long, default_value = "MNQ")]
    preset: String,
    /// Read historical civil labels against the preset's fixed offset
    /// (`civil`, the default) or as instants (`instant`). Civil is the
    /// default because it lands CST and CDT observations on the same
    /// canonical session phase, so no season disproportionately supplies the
    /// boundary buckets.
    #[arg(long, default_value = "civil")]
    alignment: String,
}

const DEFAULT_ARCHIVE: &str = "research/market-data/nq-1m_bk.zip";

fn alignment_of(name: &str) -> anyhow::Result<Alignment> {
    match name {
        "civil" => Ok(Alignment::Civil),
        "instant" => Ok(Alignment::Instant),
        other => anyhow::bail!("unknown alignment {other}; expected civil or instant"),
    }
}

pub(crate) fn run(command: SessionProfileCommand) -> anyhow::Result<()> {
    let (args, is_fit) = match command {
        SessionProfileCommand::Preflight(args) => (args, false),
        SessionProfileCommand::Fit(args) => (args, true),
    };
    let archive = args
        .archive
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ARCHIVE));
    let alignment = alignment_of(&args.alignment)?;
    let report = if is_fit {
        mogwai_lab::session_profile::fit_report(&archive, alignment, &args.preset)
    } else {
        mogwai_lab::session_profile::preflight_report(&archive, alignment, &args.preset)
    }
    .map_err(|e| anyhow::anyhow!("session profile refused: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
