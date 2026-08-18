// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai segments` - the session-segment sampler's two offline halves.
//!
//! `cut` carves session slices out of a delivered TBBO month into a returns-space
//! segment library; `tape` composes that library into an endless single-session
//! tape and dumps it as CSV for charting. Together they are Slice 1 of
//! `notes/segment-sampler.md`: cut, re-anchor, loop, chart.
//!
//! The bars dump deliberately emits the SAME header `mogwai gen --type bars`
//! does, so `analysis/plot_tape.py --csv <file>` charts a composed tape and a
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
    /// Compose a segment library into an endless tape and dump it as CSV.
    Tape(TapeArgs),
}

#[derive(Args)]
pub(crate) struct CutArgs {
    /// Instrument whose corpus to cut, e.g. MNQ.
    #[arg(long, default_value = "MNQ")]
    symbol: String,
    /// Delivered month, `YYYY-MM`.
    #[arg(long)]
    month: String,
    /// Session window to cut. `asia` is the first nine hours of the CME
    /// session, `london` the six after it.
    #[arg(long, default_value = "asia")]
    window: String,
    /// Corpus root holding `<symbol>v/<month>.<state>.tbbo`.
    #[arg(long, default_value = "research/market-data/databento")]
    root: PathBuf,
    /// Cut this directory instead of resolving one under `--root`.
    #[arg(long, conflicts_with = "root")]
    dir: Option<PathBuf>,
    /// Timezone authority the session calendar resolves against.
    #[arg(long, default_value = "analysis/tz-america-chicago-2026c.json")]
    tz_authority: PathBuf,
    /// Where to write the library JSON.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum TapeType {
    Trades,
    Bars,
}

#[derive(Args)]
pub(crate) struct TapeArgs {
    /// Segment library written by `segments cut`.
    #[arg(long)]
    library: PathBuf,
    /// What to emit.
    #[arg(long = "type", value_enum, default_value = "bars")]
    kind: TapeType,
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
    /// The tape's price level anchor. An integration constant: it scales the
    /// whole tape and changes no return.
    #[arg(long, default_value_t = 20_000.0)]
    start_price: f64,
    /// First tick's timestamp, unix ns.
    #[arg(long, default_value_t = 0)]
    start: u64,
    /// Dead time inserted at each segment seam, in seconds.
    #[arg(long, default_value_t = 1)]
    seam_gap_s: u64,
    /// Suppress each segment's measured reopen gap, yielding a continuous tape
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
        SegmentsCommand::Tape(args) => tape(args),
    }
}

fn cut(args: CutArgs) -> anyhow::Result<()> {
    let window = segments::window_by_name(&args.window)?;
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
    let library = segments::cut(&dir, &args.symbol, &args.month, window, &frame)?;

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
fn tape(args: TapeArgs) -> anyhow::Result<()> {
    let library = SegmentLibrary::load(&args.library)
        .with_context(|| format!("loading {}", args.library.display()))?;
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
        TapeType::Trades => write_trades(&mut source, args.ticks, &mut sink)?,
        TapeType::Bars => {
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

/// Bars over the composed tape, in the header `mogwai gen --type bars` emits so
/// `analysis/plot_tape.py` charts both without a flag.
///
/// Unlike the generator's writer this does NOT fill empty windows: a composed
/// endless tape has no calendar and therefore no scheduled desert to render -
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
