// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai segments` - the session-segment sampler's two offline halves.
//!
//! `cut` carves session slices out of a delivered TBBO month into a returns-space
//! segment library; `compose` realizes that library as an endless single-session
//! river and dumps it as CSV for charting. Together they are Slice 1 of
//! `notes/segment-sampler.md`: cut, re-anchor, loop, chart.
//!
//! The bars dump deliberately emits the SAME header `mogwai gen --type bars`
//! does, so `analysis/plot_tape.py --csv <file>` charts a composed river and a
//! generated one with one tool and no flags in between. Comparing the two in
//! the same viewer is the eyeball gate this whole direction is judged by.

use std::io::Write;
use std::num::NonZeroU64;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Subcommand, ValueEnum};
use mogwai_data::segment::{SegmentCompose, SegmentLibrary, SegmentSource};
use mogwai_data::{BarAcc, TickEvent, TickSource, fold_trade};
use mogwai_lab::segments;
use mogwai_lab::session::ScheduleFrame;

#[derive(Subcommand)]
pub(crate) enum SegmentsCommand {
    /// Cut a delivered TBBO month into a returns-space segment library.
    Cut(CutArgs),
    /// Realize a segment library as an endless river and dump it as CSV.
    Compose(ComposeArgs),
}

#[derive(Args)]
pub(crate) struct CutArgs {
    /// Instrument whose corpus to cut, e.g. MNQ.
    #[arg(long, default_value = "MNQ")]
    symbol: String,
    /// Delivered month, `YYYY-MM`.
    #[arg(long)]
    month: String,
    /// Session window to cut, as a slice of the CME session measured from the
    /// reopen.
    #[arg(long, value_enum, default_value_t = WindowArg::Asia)]
    window: WindowArg,
    /// Corpus root holding `<symbol>v/<month>.<state>.tbbo`.
    #[arg(long, default_value = "research/market-data/databento")]
    root: PathBuf,
    /// Cut this directory instead of resolving one under `--root`.
    #[arg(long, conflicts_with = "root")]
    dir: Option<PathBuf>,
    /// Timezone authority the session calendar resolves against.
    #[arg(long, default_value = "analysis/tz-america-chicago-2026c.json")]
    tz_authority: PathBuf,
    /// Drop a session carrying fewer ticks than this fraction of the month's
    /// median session. Holiday half-sessions are non-empty but compose as
    /// stubs, and sampling draws them as often as a full day. 0 keeps every
    /// non-empty slice.
    #[arg(long, default_value_t = mogwai_lab::segments::DEFAULT_MIN_TICKS_FRACTION)]
    min_ticks_fraction: f64,
    /// Where to write the library JSON.
    #[arg(long)]
    out: PathBuf,
}

/// The CLI spelling of `mogwai_lab::segments::WINDOWS`.
///
/// A separate type rather than a `ValueEnum` on `SessionWindow` because
/// `mogwai-lab` carries no clap dependency, and because `SessionWindow` is a
/// struct of offsets rather than an enum. The two lists cannot drift:
/// `every_cuttable_window_has_a_command_line_spelling` holds this enum's
/// resolved names against `WINDOWS` in both directions, which is the pinning
/// the previous `String` plus `window_by_name` had no way to do - an added
/// window was simply unreachable from `--window`, and `--help` enumerated
/// nothing at all.
#[derive(Copy, Clone, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum WindowArg {
    /// The first nine hours of the session, exchange-local 17:00 to 02:00.
    Asia,
    /// Exchange-local 02:00 to 08:00, the six hours after Asia.
    London,
    /// Exchange-local 08:00 to 11:00, the cash open with a half-hour lead-in.
    NyMorning,
    /// Exchange-local 09:30 to 15:00, the second half of the cash session.
    NyAfternoon,
}

impl WindowArg {
    fn resolve(self) -> segments::SessionWindow {
        match self {
            WindowArg::Asia => segments::ASIA,
            WindowArg::London => segments::LONDON,
            WindowArg::NyMorning => segments::NY_MORNING,
            WindowArg::NyAfternoon => segments::NY_AFTERNOON,
        }
    }
}

#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum ComposeType {
    Trades,
    Bars,
}

#[derive(Args)]
pub(crate) struct ComposeArgs {
    /// Segment library written by `segments cut`.
    #[arg(long)]
    library: PathBuf,
    /// What to emit.
    #[arg(long = "type", value_enum, default_value = "bars")]
    kind: ComposeType,
    /// Bar interval in seconds. Bars mode only.
    #[arg(long, default_value_t = 60)]
    interval_s: u64,
    /// How many ticks to compose. The source is endless, so a dump is bounded
    /// by count rather than by exhaustion.
    #[arg(long, default_value_t = 2_000_000)]
    ticks: u64,
    /// Symbol the composed ticks are published under.
    #[arg(long, default_value = "MNQ")]
    symbol: String,
    /// Draw-order seed.
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// The river's price level anchor. An integration constant: it scales the
    /// whole river and changes no return.
    #[arg(long, default_value_t = 20_000.0)]
    start_price: f64,
    /// First tick's timestamp, unix ns.
    #[arg(long, default_value_t = 0)]
    start: u64,
    /// Dead time inserted at each segment seam, in seconds.
    #[arg(long, default_value_t = 1)]
    seam_gap_s: u64,
    /// Suppress each segment's measured reopen gap, yielding a continuous river
    /// with no gaps at all - the A/B against the fitted generator, which
    /// produces none.
    #[arg(long)]
    no_reopen_gaps: bool,
    /// Cycle segments in library order instead of sampling with replacement.
    #[arg(long)]
    in_order: bool,
    /// Write CSV here instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

pub(crate) fn run(command: SegmentsCommand) -> anyhow::Result<()> {
    match command {
        SegmentsCommand::Cut(args) => cut(args),
        SegmentsCommand::Compose(args) => compose(args),
    }
}

fn cut(args: CutArgs) -> anyhow::Result<()> {
    let window = args.window.resolve();
    let dir = match args.dir {
        Some(dir) => dir,
        None => segments::corpus_dir(&args.root, &args.symbol, &args.month)?,
    };
    let frame = ScheduleFrame::stage_m(&args.tz_authority).with_context(|| {
        format!(
            "loading the timezone authority at {}",
            args.tz_authority.display()
        )
    })?;

    eprintln!(
        "cutting {} {} {} segments from {}",
        args.symbol,
        args.month,
        window.name,
        dir.display()
    );
    // Elapsed is emitted by the command itself rather than left to an external
    // stopwatch. The cut is the expensive surface here - a delivered month is
    // multiple GB decompressed - and the decision it informs is whether cutting
    // a full eleven-month corpus is a coffee break or an overnight job, which
    // the standing rule says to answer before running it rather than after.
    let started = std::time::Instant::now();
    let (library, dropped) = segments::cut_with(
        &dir,
        &args.symbol,
        &args.month,
        window,
        &frame,
        args.min_ticks_fraction,
    )?;

    // Named, not counted. A month losing a session is something the operator
    // has to be able to check against the calendar, and a bare count cannot be
    // checked against anything.
    for drop in &dropped {
        eprintln!(
            "dropped {} ({} ticks): {}",
            drop.trade_date, drop.trade_count, drop.reason
        );
    }

    // The work size, on stderr as a sidecar-shaped counter: a wall alone cannot
    // distinguish a faster cut from a smaller one.
    let ticks: usize = library.segments.iter().map(|s| s.trade_count).sum();
    let gapped = library
        .segments
        .iter()
        .filter(|s| s.open_gap_ret.is_some())
        .count();
    eprintln!("segments_cut={}", library.segments.len());
    eprintln!("segment_ticks={ticks}");
    eprintln!("segments_with_measured_open_gap={gapped}");
    eprintln!("segments_dropped_as_thin={}", dropped.len());
    eprintln!("cut_seconds={:.1}", started.elapsed().as_secs_f64());

    if let Some(parent) = args.out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string(&library).context("serializing the segment library")?;
    std::fs::write(&args.out, text).with_context(|| format!("writing {}", args.out.display()))?;
    eprintln!("wrote {}", args.out.display());
    Ok(())
}

// By value to match the dispatch site, which moves the subcommand variant's
// payload out of the parsed `Cli`; the body only borrows.
#[allow(
    clippy::needless_pass_by_value,
    reason = "matches the by-value dispatch site; the body only borrows args internally"
)]
fn compose(args: ComposeArgs) -> anyhow::Result<()> {
    // Load and compose are timed separately because they scale differently: the
    // load is one parse of a whole month's library and grows with the corpus,
    // while the compose is linear in --ticks and does not. A single wall would
    // hide which one a slow run spent its time in.
    let load_started = std::time::Instant::now();
    let library = SegmentLibrary::load(&args.library)
        .with_context(|| format!("loading {}", args.library.display()))?;
    let load_seconds = load_started.elapsed().as_secs_f64();
    let mut config = SegmentCompose::new(&args.symbol, args.seed);
    config.start_price = args.start_price;
    config.start_ns = args.start;
    config.seam_gap_ns = args
        .seam_gap_s
        .checked_mul(1_000_000_000)
        .context("the seam gap overflows nanoseconds")?;
    config.reopen_gaps = !args.no_reopen_gaps;
    config.sample = !args.in_order;

    let mut source = SegmentSource::new(library, config)?;
    eprintln!("composing the {} window", source.window());
    let compose_started = std::time::Instant::now();

    let mut sink: Box<dyn Write> = match &args.out {
        Some(path) => {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            Box::new(std::io::BufWriter::new(
                std::fs::File::create(path)
                    .with_context(|| format!("creating {}", path.display()))?,
            ))
        }
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };

    match args.kind {
        ComposeType::Trades => write_trades(&mut source, args.ticks, &mut sink)?,
        ComposeType::Bars => {
            let interval = NonZeroU64::new(
                args.interval_s
                    .checked_mul(1_000_000_000)
                    .context("the bar interval overflows nanoseconds")?,
            )
            .context("--interval-s must be nonzero")?;
            write_bars(&mut source, args.ticks, interval, &mut sink)?;
        }
    }
    sink.flush().context("flushing the CSV")?;
    eprintln!("composed_ticks={}", args.ticks);
    // A rail that fired is not a detail: past it the printed prices are the
    // clamp rather than the integrated walk, and a wall or a tick count alone
    // cannot tell the two river realizations apart.
    eprintln!("composed_price_clamps={}", source.clamps());
    eprintln!("library_load_seconds={load_seconds:.1}");
    eprintln!(
        "compose_seconds={:.1}",
        compose_started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn write_trades(
    source: &mut SegmentSource,
    ticks: u64,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    writeln!(out, "ts_event,price,size,aggressor")?;
    for _ in 0..ticks {
        let Some(TickEvent::Trade(t)) = source.next_tick() else {
            // The composer has exactly one terminal condition, so name it
            // rather than reporting an "endless" source as merely exhausted.
            if source.clock_exhausted() {
                bail!(
                    "the composed river ran the nanosecond clock out of range; \
                     lower --start or ask for fewer --ticks"
                );
            }
            bail!("the composed source ended early; it is meant to be endless");
        };
        writeln!(
            out,
            "{},{},{},{:?}",
            t.ts_event, t.price, t.size, t.aggressor
        )?;
    }
    Ok(())
}

/// Bars over the composed river, in the header `mogwai gen --type bars` emits so
/// `analysis/plot_tape.py` charts both without a flag.
///
/// Unlike the generator's writer this does NOT fill empty windows: a composed
/// endless river has no calendar and therefore no scheduled desert to render -
/// every gap in it is a real dwell inside a real session slice, or the seam,
/// and both are bounded. A fill rule here would invent bars rather than expose
/// deserts, which is the opposite of what that rule exists for.
fn write_bars(
    source: &mut SegmentSource,
    ticks: u64,
    interval: NonZeroU64,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    writeln!(
        out,
        "open_ts,close_ts,open,high,low,close,volume,trade_count"
    )?;
    let mut acc: Option<BarAcc> = None;
    for _ in 0..ticks {
        let Some(TickEvent::Trade(t)) = source.next_tick() else {
            // The composer has exactly one terminal condition, so name it
            // rather than reporting an "endless" source as merely exhausted.
            if source.clock_exhausted() {
                bail!(
                    "the composed river ran the nanosecond clock out of range; \
                     lower --start or ask for fewer --ticks"
                );
            }
            bail!("the composed source ended early; it is meant to be endless");
        };
        if let Some(closed) = fold_trade(&mut acc, t.price, t.size, t.ts_event, interval) {
            emit_bar(&closed, interval, out)?;
        }
    }
    if let Some(open) = &acc {
        emit_bar(open, interval, out)?;
    }
    Ok(())
}

/// `BarAcc` stores only the window's CLOSE instant; the open is that close
/// minus the interval it was folded at, which is exact because `fold_trade`
/// anchors windows on the epoch.
fn emit_bar(bar: &BarAcc, interval: NonZeroU64, out: &mut impl Write) -> anyhow::Result<()> {
    writeln!(
        out,
        "{},{},{},{},{},{},{},{}",
        bar.close_ts.saturating_sub(interval.get()),
        bar.close_ts,
        bar.open,
        bar.high,
        bar.low,
        bar.close,
        bar.volume,
        bar.count
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;
    use mogwai_lab::segments;

    use super::WindowArg;

    /// The `--window` enum and `WINDOWS` are one list written twice, so the
    /// identity between them is asserted rather than assumed. Both directions
    /// matter, and they fail differently: a `WindowArg` naming a window the lab
    /// does not have is an argument that parses and then refuses at run time,
    /// while a `WINDOWS` entry with no `WindowArg` is a cuttable window that
    /// `--window` cannot reach and `--help` does not list. The second is what
    /// the previous `String` argument shipped - `ny-morning` and
    /// `ny-afternoon` were cuttable but undocumented on the command line, and
    /// the argument's own help text named only two of the four.
    #[test]
    fn every_cuttable_window_has_a_command_line_spelling() {
        let spelled: Vec<&str> = WindowArg::value_variants()
            .iter()
            .map(|arg| arg.resolve().name)
            .collect();
        for name in &spelled {
            segments::window_by_name(name).unwrap_or_else(|e| {
                panic!("--window {name} must name a window the lab can cut: {e}")
            });
        }
        for window in segments::WINDOWS {
            assert!(
                spelled.contains(&window.name),
                "the cuttable window {} has no --window spelling, so nothing can ask for it",
                window.name
            );
        }
        assert_eq!(
            spelled.len(),
            segments::WINDOWS.len(),
            "--window must spell exactly the cuttable windows, no more and no fewer"
        );
    }

    /// The kebab-case rename is load-bearing: `NyMorning` resolves to the lab's
    /// `ny-morning`, and clap must ACCEPT that same string, or the enum's
    /// variant name and the lab's name would be two different command-line
    /// spellings with only one of them documented.
    #[test]
    fn the_command_line_spelling_is_the_labs_own_name() {
        for arg in WindowArg::value_variants() {
            let name = arg.resolve().name;
            let parsed = WindowArg::from_str(name, true)
                .unwrap_or_else(|e| panic!("clap must accept --window {name}: {e}"));
            assert_eq!(
                parsed.resolve().name,
                name,
                "--window {name} must parse back to the window it names"
            );
        }
    }
}
