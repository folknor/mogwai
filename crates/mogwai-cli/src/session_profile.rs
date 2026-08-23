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

use clap::{Args, Subcommand, ValueEnum};
use mogwai_lab::session_profile::{Alignment, MODEL_CLOCK_ALIGNMENT_DEFAULT};

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
    /// The calendar is an input to the estimator rather than a consumer of
    /// it, so this decides what the fit means.
    #[arg(long, default_value = "MNQ")]
    preset: String,
    /// How historical labels are read. Civil is the default because it lands
    /// CST and CDT observations on the same canonical session phase, so no
    /// season disproportionately supplies the boundary buckets.
    // NOT A DOC COMMENT: this is for the next maintainer, not the operator
    // reading `--help`. The default is the lab's
    // `MODEL_CLOCK_ALIGNMENT_DEFAULT` rather than a second declaration of it -
    // the alignment the model runs on is the lab's fact. Until 2026-08-20 it
    // was stated in both places, dead in the lab and live here.
    #[arg(long, value_enum, default_value_t = AlignmentArg::of(MODEL_CLOCK_ALIGNMENT_DEFAULT))]
    alignment: AlignmentArg,
}

/// The CLI spelling of [`Alignment`].
///
/// It is a separate type rather than a `ValueEnum` derive on the lab's enum
/// because `mogwai-lab` carries no clap dependency and must not grow one to
/// describe an argument. The pair cannot drift: [`AlignmentArg::resolve`] is
/// total in one direction and [`AlignmentArg::of`] matches exhaustively on
/// `Alignment` in the other, so a variant added to the lab fails to compile
/// here rather than becoming unreachable from the command line. The default
/// is not a third encoding: `of` is what carries the lab's
/// `MODEL_CLOCK_ALIGNMENT_DEFAULT` onto the command line.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum AlignmentArg {
    /// Read historical civil labels against the preset's fixed offset.
    Civil,
    /// Read historical labels as instants.
    Instant,
}

impl AlignmentArg {
    /// The spelling of a lab alignment. Exhaustive by construction: a variant
    /// added to `Alignment` fails this match to compile, so the new alignment
    /// cannot ship without a command-line name.
    const fn of(alignment: Alignment) -> Self {
        match alignment {
            Alignment::Civil => AlignmentArg::Civil,
            Alignment::Instant => AlignmentArg::Instant,
        }
    }

    fn resolve(self) -> Alignment {
        match self {
            AlignmentArg::Civil => Alignment::Civil,
            AlignmentArg::Instant => Alignment::Instant,
        }
    }
}

const DEFAULT_ARCHIVE: &str = "research/market-data/nq-1m_bk.zip";

pub(crate) fn run(command: SessionProfileCommand) -> anyhow::Result<()> {
    let (args, is_fit) = match command {
        SessionProfileCommand::Preflight(args) => (args, false),
        SessionProfileCommand::Fit(args) => (args, true),
    };
    let archive = args
        .archive
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ARCHIVE));
    let alignment = args.alignment.resolve();
    let report = if is_fit {
        mogwai_lab::session_profile::fit_report(&archive, alignment, &args.preset)
    } else {
        mogwai_lab::session_profile::preflight_report(&archive, alignment, &args.preset)
    }
    .map_err(|e| anyhow::anyhow!("session profile refused: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;
    use mogwai_lab::session_profile::Alignment;

    use super::AlignmentArg;

    /// `AlignmentArg::of` is the gate, not the assertions here.
    ///
    /// `AlignmentArg` and `Alignment` are two spellings of one quantity, and
    /// the variants are the half neither side can derive from the other - a
    /// clap name is not computable from a Rust identifier - so the identity is
    /// asserted. `of` is exhaustive over `Alignment`, so adding a variant to
    /// the lab breaks the compile rather than silently leaving the new
    /// alignment with no command-line spelling, which is the failure mode the
    /// hand-rolled `match` on a `String` had, where an unknown name was a
    /// runtime bail and a new alignment was simply unreachable.
    ///
    /// The default is a different case and is not asserted here, because it is
    /// derivable and therefore derived: the argument's `default_value_t` reads
    /// the lab's `MODEL_CLOCK_ALIGNMENT_DEFAULT` through `of`. Until 2026-08-20
    /// that constant had zero readers workspace-wide while the CLI declared
    /// `Civil` a second time, so the quantity had three encodings and a gate on
    /// two of them.
    #[test]
    fn every_alignment_has_a_command_line_spelling() {
        for alignment in [Alignment::Civil, Alignment::Instant] {
            assert!(
                AlignmentArg::of(alignment).resolve() == alignment,
                "the command-line spelling must resolve back to the alignment it names"
            );
        }
        assert_eq!(
            AlignmentArg::value_variants().len(),
            2,
            "every AlignmentArg variant must be covered by the exhaustive match above"
        );
    }
}
