// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai gen` - runs the synthetic generator OFFLINE (no server, no sockets,
//! no adapter) and writes its output as CSV, either raw trades or aggregated
//! OHLCV bars, so the generated tape can be charted and inspected. Reuses the
//! server's own generation plumbing (`InstrumentProfiles`, `fingerprint()`,
//! `GeneratedSource`) and the shared bar-aggregation core
//! (`mogwai_data::{BarAcc, fold_trade}`), so the PROCESS is the shipped one.
//! The realization is not: a run draws or configures its own seed, so this CLI
//! reproduces a served walk only when handed that run's tape seed via `--seed`.
//!
//! The load-bearing piece is the empty-window-fill rule in `write_bars`, which
//! renders multi-hour trade deserts as flat zero-volume runs on a chart -
//! making them visible is why this command exists.

use std::io::Write;
use std::num::NonZeroU64;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, ValueEnum};
use mogwai_data::{BarAcc, TickEvent, TickSource, fold_trade};
use mogwai_protocol::{
    AggressorSide, ClientHavoc, ConnHavoc, HavocSpec, MarketRegime, TradeTick,
    validate_client_havoc, validate_conn_havoc, validate_divergence, validate_market_regime,
};
use rust_decimal::Decimal;

use mogwai_server::source::fingerprint;

// The summary accumulator moved to `mogwai_lab::summary` at phase 3b so the
// protocol-11 fit driver can drive it in-process. This CLI surface is
// unchanged: `--type summary` still walks the same source with the same
// profile and serializes the same struct.
pub(crate) use mogwai_lab::summary::{SummaryAcc, summarize};

/// Default offline realization, shared with the realism certification seed.
const DEFAULT_GEN_SEED: u64 = 42;

#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum GenType {
    Trades,
    Bars,
    /// One JSON object of bounded fit statistics from the full walk. The
    /// calibration instrument of the MNQ TBBO fit: consumes every draw
    /// (sizes, prices, sides, quotes) and emits only sufficient statistics.
    Summary,
    /// One JSON line per parent inside `--trace-from`/`--trace-until`: the
    /// volatility intermediates off the real `GarchVol::step` path. The
    /// forensic instrument of the generator successor spec - observation
    /// only, byte-identical tape, pinned by test.
    Trace,
    /// One JSON object with the protocol-12a generated-side per-seed
    /// record (spec Brick G): per-session Blocks 1-4 in the observed
    /// serialized shape plus the Block-5 forensic records. Consumer-only:
    /// reads events and `VolTrace`, perturbs no draws.
    Measure12a,
}

/// `mogwai gen` arguments. Dumps the offline generator as CSV.
#[derive(Args)]
pub(crate) struct GenArgs {
    /// What to emit: raw trades, or aggregated OHLCV bars.
    #[arg(long = "type", value_enum, default_value = "trades")]
    kind: GenType,
    /// Sim-time span to generate: `<n><unit>`, unit one of s m h d w mo y
    /// (mo = 30d month, y = 365d year). Row count follows from it.
    #[arg(long, default_value = "1d")]
    length: String,
    /// Bar interval, same grammar as `--length`. Required with `--type bars`,
    /// rejected with `--type trades`.
    #[arg(long)]
    interval: Option<String>,
    /// Instrument to generate. A built-in venue symbol first, then an embedded
    /// preset name (MNQ, MES, BTCUSDT). A preset brings its
    /// own session calendar, so a futures tape shows its closed weekend.
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    /// Resolve the instrument from an operator config TOML instead of a
    /// symbol, through the server's REAL `Config::load` and profile
    /// construction - the same path a served config takes - so a scratch
    /// profile with candidate scalars is expressible without touching
    /// committed presets. The file must carry `[instrument]` defaults or a
    /// `[symbols.*]` overlay matching its boot symbol.
    /// Mutually exclusive with an explicit --symbol.
    #[arg(long, value_name = "PATH", conflicts_with = "symbol")]
    config: Option<PathBuf>,
    /// Warm-up span generated BEFORE --start, same grammar as --length.
    /// Summary mode only: the walk begins at `start - warmup`, and every
    /// accumulator covers exactly `[start, start + length)` - warm-up
    /// observations are discarded, so the measurement window is the intended
    /// calendar interval with full session weighting.
    #[arg(long)]
    warmup: Option<String>,
    /// Trace window opening instant, unix ns. Trace mode only; must satisfy
    /// start <= trace-from < trace-until <= start + length.
    #[arg(long)]
    trace_from: Option<u64>,
    /// Trace window closing instant (exclusive), unix ns. Trace mode only.
    #[arg(long)]
    trace_until: Option<u64>,
    /// Walk seed. Defaults to `DEFAULT_GEN_SEED`, the realism gate's seed. The
    /// running server draws or configures its own run seed instead, so this
    /// offline walk matches a served one only when given that run's tape seed.
    #[arg(long)]
    seed: Option<u64>,
    /// Sim-time unix-ns anchor the walk starts at. Default 0 is the canonical
    /// from-origin tape (anchor-independent desert phenomenon; see the Goal).
    #[arg(long, default_value_t = 0)]
    start: u64,
    /// Override the profile's start price (the price-scale anchor).
    #[arg(long)]
    start_price: Option<Decimal>,
    /// Data-regime havoc as internally-tagged JSON, e.g.
    /// `{"type":"LiquidityDrought","thin_factor":5.0}`. Validated by
    /// `validate_market_regime`.
    #[arg(long)]
    regime: Option<String>,
    /// Read a full HavocSpec from this JSON file and apply its `data` market
    /// regime. The whole spec is validated (a file the server would reject is
    /// rejected here), but the client, conn, and server surfaces do not affect an
    /// offline tape dump and are noted on stderr. Mutually exclusive with
    /// --regime.
    #[arg(long, value_name = "PATH", conflicts_with = "regime")]
    havoc: Option<PathBuf>,
    /// Write CSV here instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

// `args` by value mirrors the dispatch site (`Command::Gen(args) =>
// r#gen::run(args)`, moving the enum variant's payload out of the parsed
// `Cli`); the body only ever borrows it, which clippy would otherwise flag as
// needless-by-value, so silenced with the reason stated.
#[allow(
    clippy::needless_pass_by_value,
    reason = "matches the by-value dispatch site; the body only borrows args internally"
)]
pub(crate) fn run(args: GenArgs) -> anyhow::Result<()> {
    let mut sink: Box<dyn Write> = match &args.out {
        Some(path) => Box::new(std::io::BufWriter::new(
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?,
        )),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };
    run_into(&args, &mut sink)?;
    sink.flush().context("flushing output")?;
    Ok(())
}

/// The control flow of `run`, minus the sink selection: parse `--length`,
/// validate the `--type`/`--interval` pairing, build the source, bound the
/// trade iterator to `[start, end)`, and dispatch to `write_trades` /
/// `write_bars`. Factored out so `run` and the `gen_run` end-to-end test share
/// one implementation instead of the test re-deriving the control flow.
fn run_into(args: &GenArgs, sink: &mut impl Write) -> anyhow::Result<()> {
    let len_ns = parse_duration(&args.length).context("parsing --length")?;
    let end = args.start.saturating_add(len_ns);

    let interval = match (args.kind, &args.interval) {
        (GenType::Bars, Some(raw)) => {
            let ns = parse_duration(raw).context("parsing --interval")?;
            Some(NonZeroU64::new(ns).context("--interval must be nonzero")?)
        }
        (GenType::Bars, None) => bail!("--type bars requires --interval"),
        (GenType::Trades | GenType::Summary | GenType::Trace | GenType::Measure12a, Some(_)) => {
            bail!("--interval is only valid with --type bars")
        }
        (GenType::Trades | GenType::Summary | GenType::Trace | GenType::Measure12a, None) => None,
    };
    if args.warmup.is_some() && !matches!(args.kind, GenType::Summary | GenType::Measure12a) {
        bail!("--warmup is only valid with --type summary or --type measure12a");
    }
    if (args.trace_from.is_some() || args.trace_until.is_some())
        && !matches!(args.kind, GenType::Trace)
    {
        bail!("--trace-from/--trace-until are only valid with --type trace");
    }

    let profile = resolve_profile_for(args)?;

    if let GenType::Trace = args.kind {
        let (Some(from), Some(until)) = (args.trace_from, args.trace_until) else {
            bail!("--type trace requires both --trace-from and --trace-until");
        };
        if !(args.start <= from && from < until && until <= end) {
            bail!(
                "trace window must satisfy start <= trace-from < trace-until \
                 <= start + length ({} <= {from} < {until} <= {end})",
                args.start
            );
        }
        let mut source = build_source(args, &profile, args.start)?;
        source.enable_vol_trace();
        return write_trace(&mut source, from, until, end, sink);
    }

    if matches!(args.kind, GenType::Summary | GenType::Measure12a) {
        let warmup_ns = match &args.warmup {
            Some(raw) => parse_duration(raw).context("parsing --warmup")?,
            None => 0,
        };
        let walk_start = args.start.checked_sub(warmup_ns).with_context(|| {
            format!(
                "--warmup {warmup_ns}ns underflows --start {}; the walk must begin at exactly start - warmup",
                args.start
            )
        })?;
        let seed = args.seed.unwrap_or(DEFAULT_GEN_SEED);
        if let GenType::Measure12a = args.kind {
            // The 12a measurement consumer is defined only over the
            // fixed CME session structure session_segment_at encodes; a
            // calendar-free profile would silently emit no sessions.
            let Some(calendar) = profile.calendar.as_ref() else {
                bail!("--type measure12a requires a session-bearing instrument profile");
            };
            let offset = calendar.utc_offset_minutes;
            let mut source = build_source(args, &profile, walk_start)?;
            source.enable_vol_trace();
            let value = run_measure12a(&mut source, &profile, seed, offset, args.start, end)?;
            serde_json::to_writer(&mut *sink, &value).context("writing measure12a JSON")?;
            writeln!(sink)?;
            return Ok(());
        }
        mogwai_lab::sidecar::marker("walk");
        let mut source = build_source(args, &profile, walk_start)?;
        let acc = summarize(&mut source, &profile, seed, args.start, end);
        // The generated-walk benchmark's work size. Both counters are
        // IDENTITY-BEARING: a summary walk is a pure function of (preset,
        // window, warmup, seed), so a run that reports a different parent or
        // row count than its baseline changed the tape, and any wall
        // comparison against that baseline is meaningless rather than
        // interesting. Emitted before the JSON is written so a run whose sink
        // fails still leaves the reading.
        mogwai_lab::sidecar::report("parents", i64::try_from(acc.parents()).unwrap_or(i64::MAX));
        mogwai_lab::sidecar::report("rows", i64::try_from(acc.sided_rows()).unwrap_or(i64::MAX));
        return write_summary(&acc, sink);
    }

    let mut source = build_source(args, &profile, args.start)?;
    let start = args.start;
    let trades = std::iter::from_fn(move || {
        loop {
            match source.next_tick() {
                Some(TickEvent::Trade(t)) => break Some(t),
                Some(TickEvent::Quote(_)) => {}
                None => break None,
            }
        }
    })
    .take_while(move |t| t.ts_event < end);

    match args.kind {
        GenType::Trades => write_trades(trades.filter(move |t| t.ts_event >= start), sink)?,
        GenType::Bars => {
            let interval = interval.expect("bars validated interval above");
            write_bars(trades, args.start, end, interval, sink)?;
        }
        GenType::Summary | GenType::Trace | GenType::Measure12a => {
            unreachable!("summary, trace and measure12a dispatched above")
        }
    }
    Ok(())
}

/// Drive the walk through `mogwai_lab::measure12a::generated::GeneratedAcc`
/// (spec Brick G), the UNIFIED block engine phase 2a landed - this CLI
/// surface used to drive a second, CLI-local twin
/// (`crate::measure12a::Measure12aAcc`, retired at phase 2c-iii); the walk
/// itself is unchanged, still the shipped one. The consumer reads events
/// and per-parent `VolTrace` records only.
pub(crate) fn run_measure12a(
    source: &mut mogwai_data::GeneratedSource,
    profile: &mogwai_server::source::InstrumentProfile,
    seed: u64,
    offset: i16,
    start: u64,
    end: u64,
) -> anyhow::Result<serde_json::Value> {
    let t0 = std::time::Instant::now();
    let mut acc = mogwai_lab::measure12a::generated::GeneratedAcc::new(
        seed,
        start,
        end,
        i32::from(offset),
        profile.scalars.modal_tick,
    );
    while let Some(event) = source.next_tick() {
        if event.ts_event() >= end {
            break;
        }
        match event {
            TickEvent::Quote(q) => {
                let trace = source.take_vol_trace();
                acc.push_quote(&q, trace)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            TickEvent::Trade(t) => acc.push_trade(&t).map_err(|e| anyhow::anyhow!("{e}"))?,
        }
    }
    let mut value = acc.finish().map_err(|e| anyhow::anyhow!("{e}"))?;
    value.as_object_mut().expect("a record").insert(
        "cost".to_string(),
        serde_json::json!({
            "walk_s": t0.elapsed().as_secs_f64(),
            "rss_bytes": self_peak_rss_bytes(),
        }),
    );
    Ok(value)
}

/// `VmHWM` (peak resident set) of THIS process, ported from the retired
/// `crate::measure12a::self_peak_rss_bytes` - the walk's own cost record,
/// distinct from `mogwai_lab::sampler::ResourceSampler`'s process-TREE
/// sampling (slice 2c-ii), which this single-shot walk has no need of.
fn self_peak_rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmHWM:").map(|rest| {
                    rest.split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0)
                        * 1024
                })
            })
        })
        .unwrap_or(0)
}

/// One JSON line per parent whose event instant falls inside
/// `[from, until)`: the parent timestamp, its child count, and the
/// volatility intermediates the source observed on the REAL step path.
/// The walk itself runs identically with the trace enabled - pinned by
/// `trace_consumes_no_draws_and_leaves_the_tape_byte_identical`.
fn write_trace(
    source: &mut mogwai_data::GeneratedSource,
    from: u64,
    until: u64,
    end: u64,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    #[derive(serde::Serialize)]
    struct TraceRecord {
        parent_ts: u64,
        child_count: u32,
        #[serde(flatten)]
        vol: mogwai_data::VolTrace,
    }

    let mut pending: Option<TraceRecord> = None;
    let emit = |record: Option<TraceRecord>, out: &mut dyn Write| -> anyhow::Result<()> {
        if let Some(record) = record
            && record.parent_ts >= from
            && record.parent_ts < until
        {
            serde_json::to_writer(&mut *out, &record).context("writing a trace record")?;
            writeln!(out)?;
        }
        Ok(())
    };
    while let Some(event) = source.next_tick() {
        let ts = event.ts_event();
        if ts >= end {
            break;
        }
        match event {
            TickEvent::Quote(q) => {
                emit(pending.take(), out)?;
                if q.ts_event >= until {
                    // Every later parent sits past the window; the walk can
                    // stop - the tape up to here is already fully realized.
                    break;
                }
                pending = source.take_vol_trace().map(|vol| TraceRecord {
                    parent_ts: q.ts_event,
                    child_count: 0,
                    vol,
                });
            }
            TickEvent::Trade(_) => {
                if let Some(record) = &mut pending {
                    record.child_count += 1;
                }
            }
        }
    }
    emit(pending.take(), out)?;
    Ok(())
}

/// `<n><unit>` -> nanoseconds. `<n>` is one-or-more ASCII digits (`>= 1`);
/// `<unit>` is EXACTLY one of `s m h d w mo y` (case-sensitive, no surrounding
/// or internal whitespace: `"1 d"`, `"1D"`, `"1"`, `"d"`, `"0d"` all error).
/// Multipliers: s=1e9, m=60e9, h=3600e9, d=86_400e9, w=604_800e9, mo=30d, y=365d.
/// Total; rejects empty/zero/unknown-unit and saturating-checks overflow to an
/// error rather than panicking.
fn parse_duration(s: &str) -> anyhow::Result<u64> {
    const NS_PER_S: u64 = 1_000_000_000;
    const NS_PER_M: u64 = 60 * NS_PER_S;
    const NS_PER_H: u64 = 3_600 * NS_PER_S;
    const NS_PER_D: u64 = 86_400 * NS_PER_S;
    const NS_PER_W: u64 = 7 * NS_PER_D;
    const NS_PER_MO: u64 = 30 * NS_PER_D;
    const NS_PER_Y: u64 = 365 * NS_PER_D;

    let digit_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (digits, unit) = s.split_at(digit_end);
    if digits.is_empty() {
        bail!("duration {s:?} has no leading digit count");
    }
    if unit.is_empty() {
        bail!("duration {s:?} has no unit");
    }
    let count: u64 = digits
        .parse()
        .with_context(|| format!("duration {s:?} has an invalid count"))?;
    if count == 0 {
        bail!("duration {s:?} must be at least 1");
    }
    let per_unit = match unit {
        "s" => NS_PER_S,
        "m" => NS_PER_M,
        "h" => NS_PER_H,
        "d" => NS_PER_D,
        "w" => NS_PER_W,
        "mo" => NS_PER_MO,
        "y" => NS_PER_Y,
        other => bail!("duration {s:?} has unknown unit {other:?}"),
    };
    count
        .checked_mul(per_unit)
        .with_context(|| format!("duration {s:?} overflows u64 nanoseconds"))
}

fn parse_regime(raw: &str) -> anyhow::Result<MarketRegime> {
    let regime: MarketRegime =
        serde_json::from_str(raw).context("parsing --regime as MarketRegime JSON")?;
    validate_market_regime(&regime).map_err(|e| anyhow::anyhow!(e))?;
    Ok(regime)
}

/// The market regime to build the generator with, from `--regime` (inline JSON)
/// or `--havoc <file>` (a HavocSpec JSON whose `data` surface is used), or
/// neither. `--regime`/`--havoc` are clap-exclusive, so at most one is set.
fn resolve_regime(args: &GenArgs) -> anyhow::Result<Option<MarketRegime>> {
    if let Some(path) = &args.havoc {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading havoc file {}", path.display()))?;
        return resolve_havoc_regime(&text)
            .with_context(|| format!("in havoc file {}", path.display()));
    }
    args.regime.as_deref().map(parse_regime).transpose()
}

/// Parse a HavocSpec from JSON `text`, validate the whole spec, note the
/// offline-inapplicable surfaces on stderr, and return the `data` regime. Pure
/// over the text (no filesystem), so tests drive it directly.
fn resolve_havoc_regime(text: &str) -> anyhow::Result<Option<MarketRegime>> {
    let spec: HavocSpec = serde_json::from_str(text).context("parsing havoc JSON")?;
    validate_client_havoc(&spec.client)
        .map_err(|e| anyhow::anyhow!("invalid client havoc: {e}"))?;
    validate_conn_havoc(&spec.conn).map_err(|e| anyhow::anyhow!("invalid conn havoc: {e}"))?;
    for div in &spec.server {
        validate_divergence(div).map_err(|e| anyhow::anyhow!("invalid server divergence: {e}"))?;
    }
    if let Some(regime) = &spec.data {
        validate_market_regime(regime).map_err(|e| anyhow::anyhow!("invalid regime: {e}"))?;
    }
    if havoc_has_offline_inapplicable_surfaces(&spec) {
        eprintln!(
            "note: --havoc applies only the data (market regime) surface offline; \
             the client, conn, and server surfaces are ignored"
        );
    }
    Ok(spec.data)
}

/// True when a loaded HavocSpec carries a surface a tape dump cannot honor.
fn havoc_has_offline_inapplicable_surfaces(spec: &HavocSpec) -> bool {
    spec.client != ClientHavoc::default()
        || spec.conn != ConnHavoc::default()
        || !spec.server.is_empty()
}

/// Resolve profile + seed + start-price override + regime and build the walk
/// at `start_ns` via the fallible constructor (a bad `--start-price` is an
/// error, not a panic).
fn build_source(
    args: &GenArgs,
    profile: &mogwai_server::source::InstrumentProfile,
    start_ns: u64,
) -> anyhow::Result<mogwai_data::GeneratedSource> {
    let fp = fingerprint();
    let seed = args.seed.unwrap_or(DEFAULT_GEN_SEED);
    let mut scalars = profile.scalars.clone();
    if let Some(p) = args.start_price {
        scalars.start_price = p;
    }
    let regime = resolve_regime(args)?;

    // The calendar is NOT optional dressing: without it a session-bearing
    // instrument prints straight through its own closed weekend and daily
    // maintenance halt, so the dump misrepresents the very tape it exists to
    // show. The served path (`source::generator`) has always applied it; this
    // command did not, which meant an MNQ chart would have been wrong even once
    // the symbol resolved. It is now a construction input rather than something
    // applied afterwards, so forgetting it is no longer expressible.
    mogwai_data::GeneratedSource::try_new_with_session_profile(
        scalars,
        seed,
        start_ns,
        fp,
        &profile.session,
        regime,
        mogwai_data::SizeGrid::from_def(&profile.def),
        profile.calendar.clone(),
    )
    .map_err(|e| anyhow::anyhow!("building the generator: {e:?}"))
}

/// The instrument to generate: `--config` resolves an operator config through
/// the server's real loading path; otherwise `--symbol` resolves a built-in
/// venue symbol first, then an embedded preset of that name.
fn resolve_profile_for(args: &GenArgs) -> anyhow::Result<mogwai_server::source::InstrumentProfile> {
    if let Some(path) = &args.config {
        return profile_from_config(path);
    }
    resolve_profile(&args.symbol)
}

/// One instrument profile from an operator config file, through the SAME
/// `Config::load` and instrument-profile construction a served run boots
/// with, so a scratch config exercises exactly the shipped validation and
/// defaulting. The config must configure an instrument: an absent table
/// resolves to DEFAULT_PRESET and would silently ignore every scratch scalar.
fn profile_from_config(
    path: &std::path::Path,
) -> anyhow::Result<mogwai_server::source::InstrumentProfile> {
    let cfg = mogwai_server::config::Config::load(Some(path.to_path_buf()))
        .with_context(|| format!("loading --config {}", path.display()))?;
    if cfg.boot_symbol_carries_no_knobs() {
        bail!(
            "--config {} carries no [instrument] or matching [symbols.*] knobs for its boot \
             symbol; it would ignore every scratch scalar, so a scratch profile must configure one",
            path.display()
        );
    }
    let profiles = mogwai_server::config::build_instrument_profiles(&cfg)?;
    let def = profiles.boot_symbol_def(cfg.boot_symbol())?;
    Ok((*profiles
        .configured(&def.symbol)
        .expect("just listed this symbol"))
    .clone())
}

/// Every symbol resolves through the shipped preset registry. An unmatched
/// string uses the default bundle under its own symbol, and BTCUSDT renders the
/// fitted BTCUSDT preset.
fn resolve_profile(symbol: &str) -> anyhow::Result<mogwai_server::source::InstrumentProfile> {
    mogwai_server::config::profile_for_symbol(symbol)
}

fn aggressor_word(side: AggressorSide) -> &'static str {
    match side {
        AggressorSide::Buyer => "buyer",
        AggressorSide::Seller => "seller",
        AggressorSide::NoAggressor => "none",
    }
}

/// Stream trade rows for `ts_event` in `[start, end)`:
/// header `ts_event,price,size,aggressor`, then one row per trade. Takes an
/// iterator of trades (not a concrete source) so tests feed a crafted sequence.
fn write_trades(
    trades: impl Iterator<Item = TradeTick>,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    writeln!(out, "ts_event,price,size,aggressor")?;
    for t in trades {
        writeln!(
            out,
            "{},{},{},{}",
            t.ts_event,
            t.price,
            t.size,
            aggressor_word(t.aggressor)
        )?;
    }
    Ok(())
}
fn write_summary(acc: &SummaryAcc, out: &mut impl Write) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(&mut *out, acc).context("writing summary JSON")?;
    writeln!(out)?;
    Ok(())
}

fn write_bar_row(out: &mut impl Write, open_ts: u64, bar: &BarAcc) -> anyhow::Result<()> {
    writeln!(
        out,
        "{},{},{},{},{},{},{},{}",
        open_ts, bar.close_ts, bar.open, bar.high, bar.low, bar.close, bar.volume, bar.count
    )
    .context("writing a bar row")
}

/// Empty-window-fill state threaded across the fold, the end-flush and the
/// trailing-empties pass. `carry` is `None` until the first real bar is
/// emitted, which is what makes a LEADING desert render as absent rows rather
/// than empty ones: there is no priced bar yet to carry forward.
struct FillState {
    prev_close: Option<u64>,
    carry: Option<Decimal>,
}

impl FillState {
    fn new() -> Self {
        Self {
            prev_close: None,
            carry: None,
        }
    }

    /// Emit an empty window `[nb - iv, nb)` for each grid boundary `nb =
    /// prev_close + iv, prev_close + 2*iv, ..` while `nb <= end`, carrying the
    /// last emitted close price forward as a flat zero-volume bar. Advances
    /// `prev_close` as it emits.
    ///
    /// `target`, when `Some`, is the close of a window that is ABOUT TO BE
    /// emitted for real by the caller right after this call returns (a
    /// rotating fold, or the end-flushed still-open accumulator) - so the
    /// boundary at exactly `target` is EXCLUDED (`nb < target`), leaving that
    /// window for the caller's own real emission rather than double-counting
    /// it here. `target = None` is the unconditional trailing call, where no
    /// real emission follows: the only ceiling is `end` itself, inclusive, so
    /// a desert window whose close lands exactly on `end` still renders.
    fn fill_empty_to(
        &mut self,
        target: Option<u64>,
        end: u64,
        iv: NonZeroU64,
        out: &mut impl Write,
    ) -> anyhow::Result<()> {
        let (Some(mut pc), Some(price)) = (self.prev_close, self.carry) else {
            return Ok(());
        };
        while let Some(nb) = pc.checked_add(iv.get()) {
            if let Some(target) = target
                && nb >= target
            {
                break;
            }
            if nb > end {
                break;
            }
            let empty = BarAcc {
                open: price,
                high: price,
                low: price,
                close: price,
                volume: Decimal::ZERO,
                count: 0,
                close_ts: nb,
            };
            write_bar_row(out, nb - iv.get(), &empty)?;
            pc = nb;
            self.prev_close = Some(pc);
        }
        Ok(())
    }

    fn emit_real(
        &mut self,
        bar: &BarAcc,
        iv: NonZeroU64,
        start: u64,
        out: &mut impl Write,
    ) -> anyhow::Result<()> {
        if bar.close_ts < iv.get() || bar.close_ts - iv.get() < start {
            // Start-straddling window: dropped exactly like a leading empty.
            // `carry` stays untouched (still `None` if this is the first
            // window), so no empties precede it either.
            return Ok(());
        }
        write_bar_row(out, bar.close_ts - iv.get(), bar)?;
        self.prev_close = Some(bar.close_ts);
        self.carry = Some(bar.close);
        Ok(())
    }
}

/// Stream bar rows over the epoch-anchored grid, one row per window FULLY
/// inside `[start, end)`, INCLUDING zero-trade windows (see the empty-fill
/// rule). header `open_ts,close_ts,open,high,low,close,volume,trade_count`.
/// Takes an iterator of trades so tests can craft deserts at chosen positions
/// (the real generator's deserts emerge hours in, seed-dependently, and cannot
/// be dialed).
fn write_bars(
    trades: impl Iterator<Item = TradeTick>,
    start: u64,
    end: u64,
    interval: NonZeroU64,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    writeln!(
        out,
        "open_ts,close_ts,open,high,low,close,volume,trade_count"
    )?;

    let mut state = FillState::new();
    let mut acc: Option<BarAcc> = None;

    for t in trades {
        if let Some(closed) = fold_trade(&mut acc, t.price, t.size, t.ts_event, interval) {
            state.fill_empty_to(Some(closed.close_ts), end, interval, out)?;
            state.emit_real(&closed, interval, start, out)?;
        }
    }

    // End-flush the still-open accumulator, if any.
    if let Some(open) = &acc
        && open.close_ts <= end
    {
        state.fill_empty_to(Some(open.close_ts), end, interval, out)?;
        state.emit_real(open, interval, start, out)?;
    }
    // If `open.close_ts > end` it straddles `end` and is DROPPED (not emitted).

    // Trailing empties, UNCONDITIONALLY: fills a desert between the last
    // emitted bar and the span end regardless of whether the open accumulator
    // flushed or dropped.
    state.fill_empty_to(None, end, interval, out)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // The rest of the moved summary surface is exercised by these tests
    // alone; the shipping paths need only `summarize`/`SummaryAcc`.
    use mogwai_lab::summary::{
        DISPLACEMENT_BIN_TICKS, SUMMARY_VOL_HORIZONS_S, SessionSegment, TOP_MINUTE_RECORDS,
        rank_top_minutes, session_segment_at,
    };

    fn tt(ts: u64, price: &str, size: &str) -> TradeTick {
        TradeTick {
            symbol: "BTCUSDT".into(),
            price: price.parse().expect("decimal price"),
            size: size.parse().expect("decimal size"),
            aggressor: AggressorSide::NoAggressor,
            ts_event: ts,
        }
    }

    fn iv(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("nonzero interval")
    }

    fn lines_of(buf: &[u8]) -> Vec<String> {
        String::from_utf8(buf.to_vec())
            .expect("utf8 csv")
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn parse_duration_units_and_multi_digit() {
        assert_eq!(parse_duration("1s").expect("s"), 1_000_000_000);
        assert_eq!(parse_duration("1m").expect("m"), 60_000_000_000);
        assert_eq!(parse_duration("1h").expect("h"), 3_600_000_000_000);
        assert_eq!(parse_duration("1d").expect("d"), 86_400_000_000_000);
        assert_eq!(parse_duration("1w").expect("w"), 604_800_000_000_000);
        assert_eq!(parse_duration("1mo").expect("mo"), 30 * 86_400_000_000_000);
        assert_eq!(parse_duration("1y").expect("y"), 365 * 86_400_000_000_000);
        assert_eq!(
            parse_duration("12h").expect("multi-digit"),
            12 * 3_600_000_000_000
        );
    }

    #[test]
    fn parse_duration_rejects_malformed_input() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("0d").is_err());
        assert!(parse_duration("1").is_err());
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("1 d").is_err());
        assert!(parse_duration("1D").is_err());
        assert!(parse_duration("1x").is_err());
        assert!(parse_duration("99999999999999999999d").is_err());
        assert!(parse_duration(&format!("{}d", u64::MAX)).is_err());
    }

    // (a) LEADING gap: `start` a boundary, first trade several windows in.
    // Asserts NO leading empty rows; the first row is the first fully-in-range
    // window.
    #[test]
    fn gen_bars_leading_gap_emits_no_leading_empties() {
        let interval = iv(100);
        let trades = vec![tt(350, "100", "1"), tt(360, "101", "1")];
        let mut buf = Vec::new();
        // `end = 400`, boundary-aligned to the window the trades land in, so
        // this test isolates the leading-gap behavior from the (separately
        // tested) trailing-fill behavior.
        write_bars(trades.into_iter(), 0, 400, interval, &mut buf).expect("write bars");
        let lines = lines_of(&buf);

        assert_eq!(
            lines[0],
            "open_ts,close_ts,open,high,low,close,volume,trade_count"
        );
        // First trade at ts=350 falls in window [300,400); nothing before it.
        assert_eq!(lines.len(), 2, "no leading empty rows: {lines:?}");
        assert_eq!(lines[1], "300,400,100,101,100,101,2,2");
    }

    // (b) INTERIOR desert: trades, gap of several windows, trades. Asserts
    // carry-forward empties with volume=0,count=0 and the carried price.
    #[test]
    fn gen_bars_interior_desert_carries_forward_empties() {
        let interval = iv(100);
        let trades = vec![
            tt(50, "100", "1"),  // window [0,100)
            tt(450, "200", "1"), // window [400,500) - three empty windows between
        ];
        let mut buf = Vec::new();
        // `end = 500`, boundary-aligned right after the second trade's
        // window, so this test isolates the interior-fill behavior from the
        // (separately tested) trailing-fill behavior.
        write_bars(trades.into_iter(), 0, 500, interval, &mut buf).expect("write bars");
        let lines = lines_of(&buf);

        assert_eq!(lines[1], "0,100,100,100,100,100,1,1");
        assert_eq!(lines[2], "100,200,100,100,100,100,0,0");
        assert_eq!(lines[3], "200,300,100,100,100,100,0,0");
        assert_eq!(lines[4], "300,400,100,100,100,100,0,0");
        assert_eq!(lines[5], "400,500,200,200,200,200,1,1");
        assert_eq!(lines.len(), 6, "no unexpected trailing rows: {lines:?}");
    }

    // (c) TRAILING desert to `end` with the last window's rotating trade
    // absent: asserts the END-FLUSH of the complete window plus trailing
    // empties.
    #[test]
    fn gen_bars_trailing_desert_end_flushes_and_fills_to_end() {
        let interval = iv(100);
        // Two trades both land in [0,100); no further trade ever rotates the
        // window, so it must be end-flushed, then empties fill to `end`.
        let trades = vec![tt(10, "100", "1"), tt(50, "150", "1")];
        let mut buf = Vec::new();
        write_bars(trades.into_iter(), 0, 400, interval, &mut buf).expect("write bars");
        let lines = lines_of(&buf);

        assert_eq!(lines[1], "0,100,100,150,100,150,2,2");
        assert_eq!(lines[2], "100,200,150,150,150,150,0,0");
        assert_eq!(lines[3], "200,300,150,150,150,150,0,0");
        assert_eq!(lines[4], "300,400,150,150,150,150,0,0");
        assert_eq!(lines.len(), 5);
    }

    // (d) STRADDLE case: a late trade whose window close_ts > end. Assert its
    // window is dropped AND the interior empties before it (all closing <=
    // end) still render (the B2 regression).
    #[test]
    fn gen_bars_straddle_drop_still_fills_interior_empties() {
        let interval = iv(100);
        let trades = vec![
            tt(10, "100", "1"),  // window [0,100)
            tt(380, "999", "1"), // rotates at ts=380, window close_ts=400 <= end=350? no.
        ];
        // end=350: the second trade's window is [300,400), close_ts=400 > end,
        // so it straddles and is dropped. But folding it still rotates and
        // closes the FIRST window [0,100) at ts=380 (>= its close_ts of 100),
        // whose close (100) is <= end, so it renders, and the interior empties
        // between [100,300) must render too.
        let mut buf = Vec::new();
        write_bars(trades.into_iter(), 0, 350, interval, &mut buf).expect("write bars");
        let lines = lines_of(&buf);

        assert_eq!(lines[1], "0,100,100,100,100,100,1,1");
        assert_eq!(lines[2], "100,200,100,100,100,100,0,0");
        assert_eq!(lines[3], "200,300,100,100,100,100,0,0");
        // The straddling window [300,400) (opened by the ts=380 trade) is
        // dropped by the end-flush check (close_ts=400 > end=350).
        assert_eq!(lines.len(), 4, "straddling window dropped: {lines:?}");
    }

    // (e) A window whose open < start (non-boundary `start`) is dropped,
    // keeping the contract "fully inside [start, end)".
    #[test]
    fn gen_bars_start_straddle_is_dropped() {
        let interval = iv(100);
        // start=50: the first window a trade at ts=10 falls into is [0,100),
        // whose open (0) is < start (50), so it must be dropped - not
        // emitted, and no empties precede the next in-range window either.
        let trades = vec![
            tt(10, "100", "1"),  // window [0,100), open 0 < start 50: dropped
            tt(150, "200", "1"), // window [100,200), open 100 >= start 50: kept
        ];
        let mut buf = Vec::new();
        write_bars(trades.into_iter(), 50, 250, interval, &mut buf).expect("write bars");
        let lines = lines_of(&buf);

        assert_eq!(lines.len(), 2, "start-straddling window dropped: {lines:?}");
        assert_eq!(lines[1], "100,200,200,200,200,200,1,1");
    }

    #[test]
    fn gen_trades_emits_header_and_ordered_rows_with_lossless_decimals() {
        let trades = vec![
            TradeTick {
                symbol: "BTCUSDT".into(),
                price: "100.070".parse().expect("decimal"),
                size: "0.333".parse().expect("decimal"),
                aggressor: AggressorSide::Buyer,
                ts_event: 10,
            },
            TradeTick {
                symbol: "BTCUSDT".into(),
                price: "99.93".parse().expect("decimal"),
                size: "1.5".parse().expect("decimal"),
                aggressor: AggressorSide::Seller,
                ts_event: 20,
            },
            TradeTick {
                symbol: "BTCUSDT".into(),
                price: "100".parse().expect("decimal"),
                size: "1".parse().expect("decimal"),
                aggressor: AggressorSide::NoAggressor,
                ts_event: 30,
            },
        ];
        let mut buf = Vec::new();
        write_trades(trades.into_iter(), &mut buf).expect("write trades");
        let lines = lines_of(&buf);

        assert_eq!(lines[0], "ts_event,price,size,aggressor");
        assert_eq!(lines[1], "10,100.070,0.333,buyer");
        assert_eq!(lines[2], "20,99.93,1.5,seller");
        assert_eq!(lines[3], "30,100,1,none");
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn gen_reproduces_the_default_profile_walk_via_build_source() {
        let args = GenArgs {
            kind: GenType::Trades,
            length: "1d".to_string(),
            interval: None,
            symbol: "BTCUSDT".to_string(),
            seed: None,
            start: 0,
            start_price: None,
            config: None,
            warmup: None,
            trace_from: None,
            trace_until: None,
            regime: None,
            havoc: None,
            out: None,
        };
        let profile = resolve_profile_for(&args).expect("cli profile");
        let mut cli_source = build_source(&args, &profile, args.start).expect("cli source");

        let fp = fingerprint();
        let profiles = mogwai_server::source::InstrumentProfiles::from_profiles(vec![
            mogwai_server::config::profile_for_symbol("BTCUSDT")
                .expect("BTCUSDT preset must resolve"),
        ]);
        let profile = profiles.configured("BTCUSDT").expect("BTCUSDT profile");
        let mut direct_source = mogwai_data::GeneratedSource::new_with_session_profile(
            profile.scalars.clone(),
            DEFAULT_GEN_SEED,
            0,
            fp,
            &profile.session,
            None,
            mogwai_data::SizeGrid::from_def(&profile.def),
            profile.calendar.clone(),
        );

        for _ in 0..50 {
            let cli_tick = cli_source.next_tick().expect("cli tick");
            let direct_tick = direct_source.next_tick().expect("direct tick");
            assert_eq!(format!("{cli_tick:?}"), format!("{direct_tick:?}"));
        }
    }

    #[test]
    fn a_preset_symbol_resolves_and_carries_its_calendar() {
        // MNQ is not in the built-in venue, so this only works through the
        // preset fallback. The calendar assertion is the load-bearing half: a
        // CME instrument generated WITHOUT its calendar prints continuously,
        // which is what this command used to do.
        let profile = resolve_profile("MNQ").expect("MNQ resolves from the embedded preset");
        assert_eq!(profile.def.symbol.as_ref(), "MNQ");
        assert_eq!(profile.def.price_increment, Decimal::new(25, 2));
        let calendar = profile
            .calendar
            .as_ref()
            .expect("the MNQ preset ships a CME calendar");
        // Saturday is genuinely shut. Derived rather than guessed: the calendar
        // maps a UTC instant to a local week minute as
        // `utc_minute + utc_offset_minutes + 5760 (mod 10080)`, where the 5760
        // places the epoch Thursday at day index 4 of a Sunday-first week. So
        // Saturday 04:00 local is week minute 6 * 1440 + 240 = 8880, and the
        // UTC minute solving that is 8880 + 300 - 5760 = 3420 - which is
        // Saturday 1970-01-03 09:00 UTC. The preset's last open window ends at
        // 8220, so 8880 sits outside every one of them.
        let saturday_04h_local_ns = 3_420 * 60 * 1_000_000_000_u64;
        assert!(
            !calendar.is_open(saturday_04h_local_ns),
            "the CME calendar must close Saturday"
        );
        // The Wednesday cash session is open, so the assertion above is about
        // the calendar rather than about everything being shut.
        let wednesday_15h_local_ns = (3_420 + 4 * 1_440 + 660) * 60 * 1_000_000_000_u64;
        assert!(
            calendar.is_open(wednesday_15h_local_ns),
            "the CME calendar must be open midweek"
        );
    }

    #[test]
    fn an_unknown_symbol_resolves_through_the_default_bundle() {
        let profile = resolve_profile("NOPE").expect("symbol resolution is total");
        assert_eq!(profile.def.symbol.as_ref(), "NOPE");
        assert!(matches!(
            profile.def.class,
            mogwai_protocol::InstrumentClass::Spot { .. }
        ));
    }

    #[test]
    fn gen_run_bars_end_to_end_over_a_boundary_aligned_span() {
        let args = GenArgs {
            kind: GenType::Bars,
            length: "1h".to_string(),
            interval: Some("5m".to_string()),
            symbol: "BTCUSDT".to_string(),
            seed: None,
            start: 0,
            start_price: None,
            config: None,
            warmup: None,
            trace_from: None,
            trace_until: None,
            regime: None,
            havoc: None,
            out: None,
        };
        let mut buf = Vec::new();
        run_into(&args, &mut buf).expect("run");
        let lines = lines_of(&buf);

        assert_eq!(
            lines[0],
            "open_ts,close_ts,open,high,low,close,volume,trade_count"
        );
        // 1h / 5m = 12 windows fully inside [0, 1h); every window fully
        // inside the span is emitted (real or empty), so the row count is
        // exactly 12.
        assert_eq!(lines.len(), 13, "expected 12 bar rows: {lines:?}");
        for line in &lines[1..] {
            let cols: Vec<&str> = line.split(',').collect();
            assert_eq!(cols.len(), 8, "malformed row: {line}");
            let _open_ts: u64 = cols[0].parse().expect("open_ts parses");
            let _close_ts: u64 = cols[1].parse().expect("close_ts parses");
            let _volume: Decimal = cols[6].parse().expect("volume parses");
            let _count: u64 = cols[7].parse().expect("trade_count parses");
        }
    }

    #[test]
    fn resolve_havoc_regime_string_driven_cases() {
        let ok = resolve_havoc_regime(r#"{"data":{"type":"LiquidityDrought","thin_factor":5.0}}"#)
            .expect("valid data-only spec");
        match ok {
            Some(MarketRegime::LiquidityDrought { thin_factor }) => {
                assert_eq!(thin_factor, 5.0);
            }
            other => panic!("expected LiquidityDrought, got {other:?}"),
        }

        let no_data =
            resolve_havoc_regime(r#"{"client":{"drop_prob":0.5}}"#).expect("valid client, no data");
        assert_eq!(no_data, None);

        assert!(
            resolve_havoc_regime(r#"{"data":{"type":"LiquidityDrought","thin_factor":0.0}}"#)
                .is_err(),
            "out-of-range data must error"
        );

        assert!(
            resolve_havoc_regime(r#"{"client":{"drop_prob":7.0}}"#).is_err(),
            "whole-spec validation must reject an invalid inapplicable surface"
        );

        assert!(resolve_havoc_regime("not json").is_err());

        assert!(
            resolve_havoc_regime(r#"{"data":{"type":"Nonsense"}}"#).is_err(),
            "unknown regime tag must error"
        );
    }

    #[test]
    fn havoc_file_missing_returns_err_with_context() {
        let args = GenArgs {
            kind: GenType::Trades,
            length: "1d".to_string(),
            interval: None,
            symbol: "BTCUSDT".to_string(),
            seed: None,
            start: 0,
            start_price: None,
            config: None,
            warmup: None,
            trace_from: None,
            trace_until: None,
            regime: None,
            havoc: Some(PathBuf::from(
                "does/not/exist/mogwai-gen-havoc-test-nonexistent.json",
            )),
            out: None,
        };
        let err = resolve_regime(&args).expect_err("nonexistent havoc path must error");
        assert!(
            err.chain()
                .any(|e| e.to_string().contains("reading havoc file")),
            "expected the reading-havoc-file context in the chain: {err:?}"
        );
    }

    #[test]
    fn havoc_has_offline_inapplicable_surfaces_cases() {
        assert!(!havoc_has_offline_inapplicable_surfaces(
            &HavocSpec::default()
        ));

        let client_armed = HavocSpec {
            client: ClientHavoc {
                drop_prob: 0.5,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(havoc_has_offline_inapplicable_surfaces(&client_armed));

        let server_armed = HavocSpec {
            server: vec![
                serde_json::from_str(r#"{"type":"DuplicateNextFill"}"#).expect("divergence"),
            ],
            ..Default::default()
        };
        assert!(havoc_has_offline_inapplicable_surfaces(&server_armed));

        let conn_armed = HavocSpec {
            conn: ConnHavoc {
                request_timeout_secs: 1,
                ..ConnHavoc::default()
            },
            ..Default::default()
        };
        assert!(havoc_has_offline_inapplicable_surfaces(&conn_armed));

        let data_only = HavocSpec {
            data: Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
            ..Default::default()
        };
        assert!(!havoc_has_offline_inapplicable_surfaces(&data_only));
    }

    #[test]
    fn havoc_regime_conflict_is_a_parse_error() {
        use clap::Parser as _;

        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            g: GenArgs,
        }

        let result = Wrap::try_parse_from([
            "gen",
            "--regime",
            r#"{"type":"LiquidityDrought","thin_factor":5.0}"#,
            "--havoc",
            "p",
        ]);
        assert!(
            result.is_err(),
            "--regime and --havoc together must be a parse error"
        );
    }

    // -----------------------------------------------------------------------
    // Brick G of the retired protocol-10 fit spec: the calibration instrument.
    // -----------------------------------------------------------------------

    /// Scratch configs live under the workspace target dir, never /tmp: all
    /// data lives in the project.
    fn scratch_config(name: &str, body: &str) -> PathBuf {
        let dir = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/gen-scratch-configs"
        ));
        std::fs::create_dir_all(&dir).expect("creating the scratch dir");
        let path = dir.join(name);
        std::fs::write(&path, body).expect("writing the scratch config");
        path
    }

    /// A config carrying a second `[symbols.*]` table used to bail with
    /// "resolved N instruments, expected exactly one"; profiles are plural now,
    /// so the boot shape is resolved by NAME instead of by count. The MNQ case
    /// additionally pins that an ABSENT top-level `symbol` resolves the shape
    /// the default `[instrument] preset` names, not `DEFAULT_PRESET`.
    #[test]
    fn a_scratch_config_with_a_second_symbol_table_still_resolves_its_boot_shape() {
        let path = scratch_config(
            "two-symbol-mnq.toml",
            "[instrument]\npreset = \"MNQ\"\n[balances]\n[symbols.BTCUSDT]\npreset = \"BTCUSDT\"\n",
        );
        let profile = profile_from_config(&path).expect("boot shape resolves by name");
        assert_eq!(profile.def.symbol.as_ref(), "MNQ");
    }

    #[test]
    fn a_scratch_config_profile_matches_the_served_profile() {
        // The whole point of --config is that a scratch profile takes the
        // SAME loading path a served config takes. A config that merely
        // names the MNQ preset must therefore resolve to exactly the profile
        // the embedded preset resolves to.
        let path = scratch_config(
            "plain-mnq.toml",
            "[instrument]\npreset = \"MNQ\"\n[balances]\n",
        );
        let from_config = profile_from_config(&path).expect("config profile");
        let from_preset =
            mogwai_server::config::profile_from_preset("MNQ").expect("preset profile");
        assert_eq!(
            format!("{:?}", from_config.def),
            format!("{:?}", from_preset.def)
        );
        assert_eq!(
            format!("{:?}", from_config.scalars),
            format!("{:?}", from_preset.scalars)
        );
        assert_eq!(
            format!("{:?}", from_config.session),
            format!("{:?}", from_preset.session)
        );
        assert_eq!(
            format!("{:?}", from_config.calendar),
            format!("{:?}", from_preset.calendar)
        );

        let no_instrument = scratch_config("no-instrument.toml", "speed = 1.0\n");
        let err = profile_from_config(&no_instrument)
            .expect_err("a config without an instrument must refuse");
        assert!(format!("{err}").contains("[instrument]"), "{err}");
    }

    #[test]
    fn a_scratch_config_whose_only_knobs_are_for_another_symbol_is_refused() {
        let path = scratch_config(
            "wrong-symbol.toml",
            "[symbols.MNQ]\nprice_increment = \"0.25\"\n",
        );
        let error = profile_from_config(&path).unwrap_err().to_string();
        assert!(error.contains("boot symbol"), "{error}");
    }

    /// Run a short summary over the profile a scratch config resolves.
    fn summary_json_for(config_body: &str, name: &str, length_ns: u64) -> String {
        let path = scratch_config(name, config_body);
        let profile = profile_from_config(&path).expect("scratch profile");
        let mut source = mogwai_data::GeneratedSource::try_new_with_session_profile(
            profile.scalars.clone(),
            DEFAULT_GEN_SEED,
            0,
            fingerprint(),
            &profile.session,
            None,
            mogwai_data::SizeGrid::from_def(&profile.def),
            profile.calendar.clone(),
        )
        .expect("scratch source");
        let acc = summarize(&mut source, &profile, DEFAULT_GEN_SEED, 0, length_ns);
        serde_json::to_string(&acc).expect("summary json")
    }

    #[test]
    fn each_candidate_scalar_moves_the_summary() {
        // Every slot the fit will solve must demonstrably reach the
        // measurement through the scratch-config path: a candidate value the
        // summary cannot see would make its inverse solve a fiction. Each
        // case overrides ONE preset knob and asserts the summary changes -
        // except size_round_frac, which is structurally inert at MNQ's
        // declared one-contract median (the 4.3 finding), so its pair of
        // configs also raises the median to 40 and differs only in the frac.
        const WINDOW_NS: u64 = 30 * 60 * 1_000_000_000;
        let mnq = |over: &str| {
            format!("[instrument]\npreset = \"MNQ\"\n[instrument.override]\n{over}\n[balances]\n")
        };
        let baseline = summary_json_for(&mnq(""), "base-mnq.toml", WINDOW_NS);
        let mnq_cases: &[(&str, &str)] = &[
            (
                "mean_event_duration_s",
                "\"generator.mean_event_duration_s\" = 0.6",
            ),
            ("children_mean", "\"generator.children_mean\" = 3.0"),
            (
                "children_single_frac",
                "\"generator.children_single_frac\" = 0.9",
            ),
            // levels_mean must stay at or below the fitted children_mean
            // 1.1711, so the moved value sits BELOW the preset's 1.1216.
            ("levels_mean", "\"generator.levels_mean\" = 1.05"),
            (
                "latent_size_median",
                "\"generator.latent_size_median\" = \"40\"",
            ),
            ("vol_scalar", "\"generator.vol_scalar\" = 0.000005"),
            ("quoted_width", "\"generator.quoted_width.ticks\" = 3"),
            ("top_sizes_bid", "\"generator.top_sizes.bid\" = \"7\""),
            ("top_sizes_ask", "\"generator.top_sizes.ask\" = \"9\""),
            (
                "trade_displacement_ticks",
                "\"generator.trade_displacement_ticks.ticks\" = 1.5",
            ),
            ("start_price", "\"generator.start_price\" = \"30000\""),
        ];
        for (slot, over) in mnq_cases {
            let moved = summary_json_for(&mnq(over), &format!("mnq-{slot}.toml"), WINDOW_NS);
            assert_ne!(baseline, moved, "{slot} never reached the summary");
        }

        // Protocol-11 session arrays (spec Brick G2): candidate curves are
        // the values the session refit solves, so a flat override must
        // demonstrably reach the new session-cell measurements through the
        // same scratch-config path.
        let flat = |name: &str| {
            let ones = vec!["1.0"; 24].join(", ");
            format!("\"session.{name}\" = [{ones}]")
        };
        for name in ["vol_hour", "intensity_hour"] {
            let moved = summary_json_for(
                &mnq(&flat(name)),
                &format!("mnq-session-{name}.toml"),
                WINDOW_NS,
            );
            assert_ne!(
                baseline, moved,
                "session.{name} never reached the summary through the config path"
            );
        }

        // size_round_frac rides the SAME config transport as every other
        // slot, because the fit will drive it through gen --config. It is
        // structurally inert at MNQ's declared one-contract median
        // (integral_lot = 1), so both configs also raise the median to 40
        // (integral_lot = 10, snapping identifiable) and only the frac
        // differs between them - proving the frac itself reaches the
        // measurement through the real candidate path.
        let frac_base = summary_json_for(
            &mnq("\"generator.latent_size_median\" = \"40\""),
            "mnq-frac-base.toml",
            WINDOW_NS,
        );
        let frac_moved = summary_json_for(
            &mnq("\"generator.latent_size_median\" = \"40\"\n\"generator.size_round_frac\" = 0.45"),
            "mnq-frac-moved.toml",
            WINDOW_NS,
        );
        assert_ne!(
            frac_base, frac_moved,
            "size_round_frac never reached the summary through the config path"
        );
    }

    /// Brick O of the retired protocol-10 successor spec: the protocol-9
    /// tape oracle. Walks the resolved crypto preset profiles directly -
    /// quotes AND trades, every field via canonical named-separator lines -
    /// and hashes each stream with FNV-1a 64 into the committed fixture.
    /// Write-once semantics: a MISSING fixture is written only at
    /// TAPE_PROTOCOL_VERSION 9 and refused by name under any other
    /// protocol, so it can never re-bless later-protocol output; a
    /// present fixture always asserts equality. This is the frozen
    /// oracle the successor's byte-identity tests compare against.
    ///
    /// IT WAS `#[ignore]`d ON A COST THAT WAS NEVER RE-MEASURED - "walks seven
    /// 6-hour streams" - and both halves of that sentence had gone stale: the
    /// matrix is THREE rows since the ETHUSDT and SOLUSDT presets were retired,
    /// and the whole test measures comfortably inside the per-test watchdog in
    /// every sweep. The gate profile ran it regardless - it sets
    /// `include_ignored` and this test is in no skip list - so the attribute
    /// bought only its exclusion from the fast changed-files lane, on a ground
    /// measurement does not support. There is no environment or policy ground
    /// either: the fixture is committed, and the walk binds no socket and needs
    /// no corpus. Nothing survived to state, so the attribute went rather than
    /// being given a corrected cost sentence that would need re-measuring
    /// forever to stay true.
    #[test]
    fn protocol9_tape_oracle() {
        const WINDOW_NS: u64 = 6 * 3_600 * 1_000_000_000;
        let fixture_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../analysis/protocol9-tape-hashes.json"
        );
        // The ETHUSDT and SOLUSDT rows were removed with their presets
        // (owner ruling, 2026-08-09). Their generator paths were identical
        // to BTCUSDT's, but the canonical lines embed the symbol, so their
        // ORACLE hashes were distinct identity-only rows: the fixture loses
        // those four entries while retaining every distinct-dynamics stream
        // (BTCUSDT at both seeds plus the surge case).
        let matrix: &[(&str, u64, bool)] = &[
            ("BTCUSDT", 42, false),
            ("BTCUSDT", 7, false),
            ("BTCUSDT", 42, true),
        ];
        let mut hashes = serde_json::Map::new();
        for &(symbol, seed, surged) in matrix {
            let profile = resolve_profile(symbol).expect("preset resolves");
            let mut source = mogwai_data::GeneratedSource::try_new_with_session_profile(
                profile.scalars.clone(),
                seed,
                0,
                fingerprint(),
                &profile.session,
                None,
                mogwai_data::SizeGrid::from_def(&profile.def),
                profile.calendar.clone(),
            )
            .expect("oracle source");
            if surged {
                source.arm_flow_surge(3_600 * 1_000_000_000, 30 * 60 * 1_000, 2.0, 1.5);
            }
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            let mut events: u64 = 0;
            while let Some(event) = source.next_tick() {
                if event.ts_event() >= WINDOW_NS {
                    break;
                }
                events += 1;
                // Canonical serialization: every field, named separator
                // layout, Display for decimals - a STABLE contract, unlike
                // Debug text, whose derived layout could drift with a field
                // rename and silently re-key the oracle.
                let line = match &event {
                    TickEvent::Trade(t) => format!(
                        "T|{}|{}|{}|{}|{}",
                        t.symbol,
                        t.price,
                        t.size,
                        aggressor_word(t.aggressor),
                        t.ts_event
                    ),
                    TickEvent::Quote(q) => format!(
                        "Q|{}|{}|{}|{}|{}|{}",
                        q.symbol, q.bid_px, q.ask_px, q.bid_sz, q.ask_sz, q.ts_event
                    ),
                };
                for byte in line.bytes() {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            assert!(events > 0, "{symbol}-{seed}: an empty stream is no oracle");
            let name = if surged {
                format!("{symbol}-{seed}-surged")
            } else {
                format!("{symbol}-{seed}")
            };
            hashes.insert(name, serde_json::json!(format!("{hash:016x}")));
        }
        let observed = serde_json::json!({
            "tape_protocol_version": mogwai_data::TAPE_PROTOCOL_VERSION,
            "hash": "fnv1a64 over canonical TickEvent lines",
            "window_ns": WINDOW_NS,
            "surge": "start 1h, 30m, rate 2.0, children 1.5",
            "entries": hashes,
        });
        if std::path::Path::new(fixture_path).exists() {
            let frozen: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(fixture_path).expect("fixture reads"),
            )
            .expect("fixture parses");
            // The WHOLE frozen contract is asserted, not merely the entry
            // hashes: a fixture whose version, window, hash convention or
            // surge parameters silently changed is a different oracle
            // wearing this one's filename.
            assert_eq!(
                frozen["tape_protocol_version"],
                serde_json::json!(9),
                "the oracle fixture must remain the protocol-9 record"
            );
            assert_eq!(
                frozen["hash"], observed["hash"],
                "the fixture's hash convention moved"
            );
            assert_eq!(
                frozen["window_ns"], observed["window_ns"],
                "the fixture's window moved"
            );
            assert_eq!(
                frozen["surge"], observed["surge"],
                "the fixture's surge parameters moved"
            );
            assert_eq!(
                frozen["entries"], observed["entries"],
                "a crypto tape moved against the protocol-9 oracle"
            );
        } else {
            assert_eq!(
                mogwai_data::TAPE_PROTOCOL_VERSION,
                9,
                "the oracle fixture is missing and the protocol is not 9: \
                 writing now would re-bless later-protocol output, refused"
            );
            std::fs::write(
                fixture_path,
                serde_json::to_string_pretty(&observed).expect("serialize"),
            )
            .expect("fixture writes");
        }
    }

    #[test]
    fn minute_ranges_match_an_independent_bar_pass() {
        // Brick T of the successor spec: the summary's per-minute tick
        // ranges against an independent collect-then-compute pass over the
        // identical seeded walk. Minutes with at least one in-window trade
        // contribute their high-low in integer ticks; the two largest
        // OBSERVATIONS (a repeated maximum is its own second maximum) feed
        // the per-seed envelope gates.
        const WINDOW_NS: u64 = 45 * 60 * 1_000_000_000;
        let profile = resolve_profile("MNQ").expect("MNQ profile");
        let build = || {
            mogwai_data::GeneratedSource::try_new_with_session_profile(
                profile.scalars.clone(),
                DEFAULT_GEN_SEED,
                0,
                fingerprint(),
                &profile.session,
                None,
                mogwai_data::SizeGrid::from_def(&profile.def),
                profile.calendar.clone(),
            )
            .expect("source")
        };
        let mut streaming = build();
        let acc = summarize(&mut streaming, &profile, DEFAULT_GEN_SEED, 0, WINDOW_NS);
        let got = serde_json::to_value(&acc).expect("summary value");

        let mut collected = build();
        let mut per_minute: std::collections::BTreeMap<u64, (f64, f64)> =
            std::collections::BTreeMap::new();
        while let Some(event) = collected.next_tick() {
            if event.ts_event() >= WINDOW_NS {
                break;
            }
            if let TickEvent::Trade(t) = event {
                let price = f64::try_from(t.price).expect("price");
                per_minute
                    .entry(t.ts_event / 60_000_000_000)
                    .and_modify(|(lo, hi)| {
                        *lo = lo.min(price);
                        *hi = hi.max(price);
                    })
                    .or_insert((price, price));
            }
        }
        let tick = f64::try_from(profile.scalars.modal_tick).expect("tick");
        let mut hist: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        let mut ranges: Vec<u64> = Vec::new();
        for (lo, hi) in per_minute.values() {
            let range = ((hi - lo) / tick).round().max(0.0) as u64;
            *hist.entry(range.to_string()).or_insert(0) += 1;
            ranges.push(range);
        }
        ranges.sort_unstable_by(|a, b| b.cmp(a));
        assert!(!ranges.is_empty(), "the window must carry traded minutes");
        assert_eq!(
            got["minute_range_ticks_hist"],
            serde_json::to_value(&hist).unwrap()
        );
        assert_eq!(got["minute_range_max_ticks"], serde_json::json!(ranges[0]));
        assert_eq!(
            got["minute_range_second_max_ticks"],
            serde_json::json!(*ranges.get(1).unwrap_or(&0))
        );

        // Protocol-11 top-minute records (spec 4.7), recomputed with a third
        // pass carrying exact Decimal bounds, trade counts and parent counts
        // (parents by first-child minute, parents delimited by quotes).
        let mut third = build();
        let mut detail: std::collections::BTreeMap<u64, (Decimal, Decimal, u64)> =
            std::collections::BTreeMap::new();
        let mut parents_by_minute: std::collections::BTreeMap<u64, u64> =
            std::collections::BTreeMap::new();
        let mut in_parent = false;
        while let Some(event) = third.next_tick() {
            if event.ts_event() >= WINDOW_NS {
                break;
            }
            match event {
                TickEvent::Quote(_) => in_parent = false,
                TickEvent::Trade(t) => {
                    if !in_parent {
                        in_parent = true;
                        *parents_by_minute
                            .entry(t.ts_event / 60_000_000_000)
                            .or_insert(0) += 1;
                    }
                    detail
                        .entry(t.ts_event / 60_000_000_000)
                        .and_modify(|(lo, hi, n)| {
                            if t.price < *lo {
                                *lo = t.price;
                            }
                            if t.price > *hi {
                                *hi = t.price;
                            }
                            *n += 1;
                        })
                        .or_insert((t.price, t.price, 1));
                }
            }
        }
        let modal = profile.scalars.modal_tick;
        let mut expected: Vec<serde_json::Value> = detail
            .iter()
            .map(|(&minute, &(lo, hi, trade_count))| {
                let range: u64 = ((hi - lo) / modal).round().try_into().unwrap();
                let start_ns = minute * 60_000_000_000;
                serde_json::json!({
                    "minute_start_ns": start_ns,
                    "minute_end_ns": start_ns + 60_000_000_000,
                    "utc_hour": (start_ns / 1_000_000_000) % 86_400 / 3_600,
                    "range_ticks": range,
                    "parent_count": parents_by_minute.get(&minute).copied().unwrap_or(0),
                    "trade_count": trade_count,
                    "low_price": lo.normalize().to_string(),
                    "high_price": hi.normalize().to_string(),
                    "trace_from_ns": start_ns,
                    "trace_until_ns": start_ns + 60_000_000_000,
                })
            })
            .collect();
        expected.sort_by(|a, b| {
            let (ra, rb) = (a["range_ticks"].as_u64(), b["range_ticks"].as_u64());
            rb.cmp(&ra).then(
                a["minute_start_ns"]
                    .as_u64()
                    .cmp(&b["minute_start_ns"].as_u64()),
            )
        });
        // The 45-minute window populates more minutes than the capacity, so
        // truncation is exercised; the 20-minute walk in
        // summary_matches_an_independent_tick_walk exercises the under-
        // capacity case.
        assert!(
            expected.len() > TOP_MINUTE_RECORDS,
            "the window must overfill the top-minute capacity"
        );
        expected.truncate(TOP_MINUTE_RECORDS);
        assert_eq!(got["top_minutes"], serde_json::Value::Array(expected));
        // The first and second records reproduce the existing maximum
        // semantics exactly (spec 4.7).
        assert_eq!(
            got["top_minutes"][0]["range_ticks"],
            got["minute_range_max_ticks"]
        );
        assert_eq!(
            got["top_minutes"][1]["range_ticks"],
            got["minute_range_second_max_ticks"]
        );
    }

    /// 2026-07-07T01:00Z, mid-overnight-segment of the July 7 session.
    const M12A_START_NS: u64 = 1_783_386_000_000_000_000;

    fn m12a_args(start: u64, length: &str) -> GenArgs {
        GenArgs {
            kind: GenType::Measure12a,
            length: length.to_string(),
            interval: None,
            symbol: "MNQ".to_string(),
            seed: Some(7),
            start,
            start_price: None,
            config: None,
            warmup: None,
            trace_from: None,
            trace_until: None,
            regime: None,
            havoc: None,
            out: None,
        }
    }

    fn m12a_walk(start: u64, length_ns: u64) -> serde_json::Value {
        let args = m12a_args(start, "1h");
        let profile = resolve_profile_for(&args).expect("MNQ profile");
        let offset = profile
            .calendar
            .as_ref()
            .expect("calendar")
            .utc_offset_minutes;
        let mut source = build_source(&args, &profile, start).expect("source");
        source.enable_vol_trace();
        run_measure12a(&mut source, &profile, 7, offset, start, start + length_ns)
            .expect("measure12a walk")
    }

    #[test]
    fn measure12a_selection_is_deterministic() {
        // Two fresh walks of the same seed and window must serialize
        // identically once the live cost fields are normalized out: the
        // deterministic payload is seed, per_session and forensic.
        let normalize = |mut v: serde_json::Value| -> String {
            v.as_object_mut()
                .expect("measure12a object")
                .remove("cost")
                .expect("cost present");
            serde_json::to_string(&v).expect("serializable")
        };
        let one = m12a_walk(M12A_START_NS, 3_600_000_000_000);
        let records = one["forensic"]["records"]
            .as_array()
            .expect("records")
            .clone();
        assert!(
            records.iter().any(|r| r["kind"] == "extreme_range"),
            "a real walk selects an extreme: {records:?}"
        );
        assert!(
            records
                .iter()
                .all(|r| r["traced_parents"].as_u64().unwrap() >= 1),
            "every selected minute is trace-grounded"
        );
        let two = m12a_walk(M12A_START_NS, 3_600_000_000_000);
        assert_eq!(normalize(one), normalize(two));
    }

    #[test]
    fn measure12a_consumer_leaves_tape_byte_identical() {
        // A plain --type bars run against one where GeneratedAcc (the
        // unified lab engine) consumed the same walk (traces enabled): the
        // consumer must not perturb draws, so the bar bytes are identical.
        // `summary` emits no tape bytes, so bars anchor this test.
        let end = M12A_START_NS + 1_200_000_000_000; // 20 minutes
        let mut plain = Vec::new();
        {
            let mut args = m12a_args(M12A_START_NS, "20m");
            args.kind = GenType::Bars;
            args.interval = Some("1m".to_string());
            run_into(&args, &mut plain).expect("plain bars");
        }
        let args = m12a_args(M12A_START_NS, "20m");
        let profile = resolve_profile_for(&args).expect("MNQ profile");
        let offset = profile
            .calendar
            .as_ref()
            .expect("calendar")
            .utc_offset_minutes;
        let mut source = build_source(&args, &profile, M12A_START_NS).expect("source");
        source.enable_vol_trace();
        let mut acc = mogwai_lab::measure12a::generated::GeneratedAcc::new(
            7,
            M12A_START_NS,
            end,
            i32::from(offset),
            profile.scalars.modal_tick,
        );
        let mut trades: Vec<TradeTick> = Vec::new();
        while let Some(event) = source.next_tick() {
            if event.ts_event() >= end {
                break;
            }
            match event {
                TickEvent::Quote(q) => {
                    let trace = source.take_vol_trace();
                    acc.push_quote(&q, trace).expect("quote");
                }
                TickEvent::Trade(t) => {
                    acc.push_trade(&t).expect("trade");
                    if t.ts_event >= M12A_START_NS {
                        trades.push(t);
                    }
                }
            }
        }
        let _ = acc.finish().expect("finish");
        let mut consumed = Vec::new();
        write_bars(
            trades.into_iter(),
            M12A_START_NS,
            end,
            NonZeroU64::new(60_000_000_000).expect("nonzero"),
            &mut consumed,
        )
        .expect("consumed bars");
        assert_eq!(plain, consumed, "the consumer perturbed the tape");
    }

    #[test]
    fn arch_coefficients_match_the_shipped_recursion() {
        // The measure12a-local ARCH/GARCH coefficients are pinned against
        // the SHIPPED recursion via traces of a real walk: recover all
        // three parameters of candidate[i+1] = a0 + a1 * base[i]^2 +
        // b1 * sigma2[i] by least squares over the transitions, then show
        // a perturbed local coefficient fails the residual bound.
        let args = m12a_args(M12A_START_NS, "1h");
        let profile = resolve_profile_for(&args).expect("MNQ profile");
        let mut source = build_source(&args, &profile, M12A_START_NS).expect("source");
        source.enable_vol_trace();
        let end = M12A_START_NS + 1_800_000_000_000;
        let mut traces: Vec<mogwai_data::VolTrace> = Vec::new();
        while let Some(event) = source.next_tick() {
            if event.ts_event() >= end {
                break;
            }
            if matches!(event, TickEvent::Quote(_))
                && let Some(trace) = source.take_vol_trace()
            {
                traces.push(trace);
            }
        }
        let transitions: Vec<(f64, f64, f64)> = traces
            .windows(2)
            .map(|w| {
                (
                    w[1].sigma2_candidate,
                    w[0].base_return.powi(2),
                    w[0].sigma2_realized,
                )
            })
            .filter(|(y, x1, x2)| y.is_finite() && x1.is_finite() && x2.is_finite())
            .collect();
        assert!(
            transitions.len() > 500,
            "enough transitions: {}",
            transitions.len()
        );
        let mean = |f: fn(&(f64, f64, f64)) -> f64, v: &[(f64, f64, f64)]| {
            v.iter().map(f).sum::<f64>() / v.len() as f64
        };
        let var = |f: fn(&(f64, f64, f64)) -> f64, v: &[(f64, f64, f64)]| {
            let m = mean(f, v);
            v.iter().map(|t| (f(t) - m).powi(2)).sum::<f64>() / v.len() as f64
        };
        assert!(var(|t| t.1, &transitions) > 0.0, "degenerate base_return^2");
        assert!(var(|t| t.2, &transitions) > 0.0, "degenerate sigma2");
        // Centered least squares for y = a0 + a1 x1 + b1 x2: solve the
        // 2x2 system over mean-centered regressors (the raw normal
        // equations would be hopelessly ill-conditioned against the
        // intercept), then recover a0 from the means.
        let my = mean(|t| t.0, &transitions);
        let mx1 = mean(|t| t.1, &transitions);
        let mx2 = mean(|t| t.2, &transitions);
        let (mut s11, mut s12, mut s22, mut s1y, mut s2y) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        for &(y, x1, x2) in &transitions {
            let (c1, c2, cy) = (x1 - mx1, x2 - mx2, y - my);
            s11 += c1 * c1;
            s12 += c1 * c2;
            s22 += c2 * c2;
            s1y += c1 * cy;
            s2y += c2 * cy;
        }
        let det = s11 * s22 - s12 * s12;
        assert!(det != 0.0, "singular centered normal equations");
        let a1 = (s1y * s22 - s2y * s12) / det;
        let b1 = (s2y * s11 - s1y * s12) / det;
        let a0 = my - a1 * mx1 - b1 * mx2;
        assert!(
            (a1 - mogwai_lab::measure12a::generated::ARCH_12A).abs() < 1e-6,
            "recovered ARCH {a1} drifts from the local constant"
        );
        assert!(
            (b1 - mogwai_lab::measure12a::generated::GARCH_12A).abs() < 1e-6,
            "recovered GARCH {b1} drifts from the local constant"
        );
        // Scale-aware residual bound with the LOCAL constants (a0 from
        // the fit): the shipped recursion satisfies them to numerics.
        let scale = transitions
            .iter()
            .map(|&(y, ..)| y.abs())
            .fold(0.0f64, f64::max);
        let max_resid = |arch: f64, garch: f64| {
            transitions
                .iter()
                .map(|&(y, x1, x2)| (y - (a0 + arch * x1 + garch * x2)).abs())
                .fold(0.0f64, f64::max)
        };
        let bound = scale * 1e-9;
        assert!(
            max_resid(
                mogwai_lab::measure12a::generated::ARCH_12A,
                mogwai_lab::measure12a::generated::GARCH_12A
            ) <= bound,
            "the local coefficients violate the shipped recursion"
        );
        // Sensitivity: perturbing either local coefficient independently
        // fails the same bound.
        assert!(
            max_resid(
                mogwai_lab::measure12a::generated::ARCH_12A * 1.001,
                mogwai_lab::measure12a::generated::GARCH_12A
            ) > bound
        );
        assert!(
            max_resid(
                mogwai_lab::measure12a::generated::ARCH_12A,
                mogwai_lab::measure12a::generated::GARCH_12A * 1.001
            ) > bound
        );
    }

    #[test]
    fn a_warmup_past_the_start_refuses_instead_of_saturating() {
        // start - warmup must be EXACT: a saturated subtraction would
        // silently shorten the warm-up and shift the walk, so underflow is a
        // named refusal, not a clamp.
        let args = GenArgs {
            kind: GenType::Summary,
            length: "1m".to_string(),
            interval: None,
            symbol: "MNQ".to_string(),
            seed: None,
            start: 1_000,
            start_price: None,
            config: None,
            warmup: Some("1s".to_string()),
            trace_from: None,
            trace_until: None,
            regime: None,
            havoc: None,
            out: None,
        };
        let mut sink = Vec::new();
        let err = run_into(&args, &mut sink).expect_err("underflowing warmup must refuse");
        let text = format!("{err:#}");
        assert!(text.contains("underflows"), "names the underflow: {text}");
    }

    /// The independent recompute of the protocol-11 session cells from a
    /// collected event stream, as JSON comparable to the summary's
    /// `session_cells`. `segment_local` selects the CONTRACT as-of rule
    /// (rule 1: state independent per segment); passing false computes the
    /// defective global-as-of variant, which the halt-crossing test uses to
    /// prove its fixture actually distinguishes the two.
    fn independent_session_cells(
        events: &[TickEvent],
        offset: i16,
        start: u64,
        end: u64,
        segment_local: bool,
    ) -> serde_json::Value {
        #[derive(Default, Clone, Copy)]
        struct Abs {
            count: u64,
            sum_abs: f64,
            max_abs: f64,
        }
        #[derive(Default, Clone, Copy)]
        struct Hz {
            count: u64,
            sum: f64,
            sumsq: f64,
            sum_abs: f64,
            max_abs: f64,
        }
        #[derive(Default)]
        struct Cells {
            end_ns: u64,
            parents: [u64; 24],
            mid_abs: [Abs; 24],
            h60: [Hz; 24],
            h300: [Hz; 24],
        }
        struct Chunk {
            ts: u64,
            mid: f64,
            first_trade_ts: Option<u64>,
        }
        let hour_of = |ts: u64| ((ts / 1_000_000_000) % 86_400 / 3_600) as usize;
        let mut chunks: Vec<Chunk> = Vec::new();
        for e in events {
            match e {
                TickEvent::Quote(q) => chunks.push(Chunk {
                    ts: q.ts_event,
                    mid: (f64::try_from(q.bid_px).unwrap() + f64::try_from(q.ask_px).unwrap())
                        / 2.0,
                    first_trade_ts: None,
                }),
                TickEvent::Trade(t) => {
                    if let Some(c) = chunks.last_mut()
                        && c.first_trade_ts.is_none()
                    {
                        c.first_trade_ts = Some(t.ts_event);
                    }
                }
            }
        }
        let mut cells: std::collections::BTreeMap<u64, Cells> = Default::default();
        // Parent counts and the adjacent valid-mid chain, keyed by segment
        // origin so a segment change breaks it.
        let mut chain: Option<(u64, f64)> = None;
        for c in chunks.iter().filter(|c| c.first_trade_ts.is_some()) {
            let first_ts = c.first_trade_ts.unwrap();
            if first_ts < start || first_ts >= end {
                continue;
            }
            let seg = session_segment_at(first_ts, offset).expect("open parent");
            let entry = cells.entry(seg.session_start_ns).or_insert_with(|| Cells {
                end_ns: seg.session_end_ns,
                ..Default::default()
            });
            let hour = hour_of(first_ts);
            entry.parents[hour] += 1;
            if c.mid.is_finite() && c.mid > 0.0 {
                if let Some((origin, prev_mid)) = chain
                    && origin == seg.segment_origin_ns
                {
                    let abs = (c.mid / prev_mid).ln().abs();
                    let cell = &mut entry.mid_abs[hour];
                    cell.count += 1;
                    cell.sum_abs += abs;
                    if abs > cell.max_abs {
                        cell.max_abs = abs;
                    }
                }
                chain = Some((seg.segment_origin_ns, c.mid));
            }
        }
        // Horizon chains per segment: boundaries strictly inside the
        // segment, first boundary having an as-of establishes,
        // hour-crossing windows excluded, the final segment settled to
        // min(segment end, window end) INCLUSIVELY.
        let quotes: Vec<(u64, f64)> = chunks.iter().map(|c| (c.ts, c.mid)).collect();
        let mut segments: Vec<SessionSegment> = Vec::new();
        for (ts, _) in &quotes {
            if let Some(seg) = session_segment_at(*ts, offset)
                && segments.last() != Some(&seg)
            {
                segments.push(seg);
            }
        }
        for (idx, seg) in segments.iter().enumerate() {
            let last = idx + 1 == segments.len();
            let until = if last {
                seg.segment_end_ns.min(end)
            } else {
                seg.segment_end_ns
            };
            for h_ns in [60_000_000_000u64, 300_000_000_000] {
                let mut prev_mid: Option<f64> = None;
                let mut k = 1u64;
                loop {
                    let boundary = seg.segment_origin_ns + k * h_ns;
                    if boundary >= seg.segment_end_ns || boundary > until {
                        break;
                    }
                    let asof = quotes
                        .iter()
                        .rev()
                        .filter(|(ts, _)| {
                            !segment_local || session_segment_at(*ts, offset).as_ref() == Some(seg)
                        })
                        .find(|(ts, _)| *ts <= boundary)
                        .map(|(_, m)| *m);
                    match (prev_mid, asof) {
                        (Some(prev), Some(cur)) if prev > 0.0 && cur > 0.0 => {
                            let window_start = boundary - h_ns;
                            if hour_of(window_start) == hour_of(boundary) {
                                let hour = hour_of(boundary);
                                let entry =
                                    cells.entry(seg.session_start_ns).or_insert_with(|| Cells {
                                        end_ns: seg.session_end_ns,
                                        ..Default::default()
                                    });
                                let cell = if h_ns == 60_000_000_000 {
                                    &mut entry.h60[hour]
                                } else {
                                    &mut entry.h300[hour]
                                };
                                let r = (cur / prev).ln();
                                cell.count += 1;
                                cell.sum += r;
                                cell.sumsq += r * r;
                                let abs = r.abs();
                                cell.sum_abs += abs;
                                if abs > cell.max_abs {
                                    cell.max_abs = abs;
                                }
                            }
                            prev_mid = asof;
                        }
                        (None, Some(_)) => prev_mid = asof,
                        _ => {}
                    }
                    k += 1;
                }
            }
        }
        let abs_json = |a: &Abs| {
            serde_json::json!({
                "count": a.count, "sum_abs": a.sum_abs, "max_abs": a.max_abs,
            })
        };
        let hz_json = |h: &Hz| {
            serde_json::json!({
                "count": h.count, "sum": h.sum, "sumsq": h.sumsq,
                "sum_abs": h.sum_abs, "max_abs": h.max_abs,
            })
        };
        serde_json::Value::Array(
            cells
                .iter()
                .map(|(&start_ns, c)| {
                    serde_json::json!({
                        "session_start_ns": start_ns,
                        "session_end_ns": c.end_ns,
                        "complete": start_ns >= start && c.end_ns <= end,
                        "parent_count_by_hour": c.parents.to_vec(),
                        "mid_abs_by_hour":
                            c.mid_abs.iter().map(abs_json).collect::<Vec<_>>(),
                        "horizon_60_by_hour":
                            c.h60.iter().map(hz_json).collect::<Vec<_>>(),
                        "horizon_300_by_hour":
                            c.h300.iter().map(hz_json).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }

    #[test]
    fn halt_boundaries_never_borrow_the_pre_halt_mid() {
        // The rule-1 regression (protocol-11 spec 4.6): fixed-horizon state
        // is independent per segment, so a pre-halt quote must never
        // establish or price a post-halt boundary. The generated tape
        // structurally resumes AT the reopen instant (the arrival clock
        // jumps to next_open), so the leak window - a post-halt boundary
        // preceding the first post-halt quote - is built by hand on a
        // MemorySource: a pre-halt parent at 20:12, then nothing until
        // 20:41, leaving the 20:31-20:40 60 s boundaries and the 20:35 and
        // 20:40 300 s boundaries quoteless. A global as-of would establish
        // and price them from the 20:12 mid; the contract skips them. The
        // global-variant recompute must DIFFER, proving the fixture bites.
        const START_NS: u64 = 1_783_455_000_000_000_000; // 20:10Z Jul 7
        const END_NS: u64 = 1_783_457_400_000_000_000; // 20:50Z
        let minute = 60_000_000_000u64;
        let profile = resolve_profile("MNQ").expect("MNQ profile");
        let tick = profile.scalars.modal_tick;
        let px = |ticks: i64| {
            Decimal::try_from(23_000.0).unwrap() + tick * Decimal::try_from(ticks as f64).unwrap()
        };
        let one = Decimal::ONE;
        let quote = |ts: u64, level: i64| {
            TickEvent::Quote(mogwai_protocol::QuoteTick {
                symbol: "MNQ".into(),
                bid_px: px(level - 1),
                ask_px: px(level + 1),
                bid_sz: one,
                ask_sz: one,
                ts_event: ts,
            })
        };
        let trade = |ts: u64, level: i64| {
            TickEvent::Trade(TradeTick {
                symbol: "MNQ".into(),
                price: px(level),
                size: one,
                aggressor: AggressorSide::Buyer,
                ts_event: ts,
            })
        };
        let events = vec![
            quote(START_NS + 2 * minute, 0), // 20:12, pre-halt
            trade(START_NS + 2 * minute, 0),
            quote(START_NS + 31 * minute, 4), // 20:41, post-halt
            trade(START_NS + 31 * minute, 4),
            quote(START_NS + 33 * minute, 8), // 20:43
            trade(START_NS + 33 * minute, 8),
        ];
        let mut source = mogwai_data::MemorySource::new(events.clone());
        let acc = summarize(&mut source, &profile, DEFAULT_GEN_SEED, START_NS, END_NS);
        let got = serde_json::to_value(&acc).expect("summary value");
        let offset = profile
            .calendar
            .as_ref()
            .expect("MNQ calendar")
            .utc_offset_minutes;
        let local = independent_session_cells(&events, offset, START_NS, END_NS, true);
        let global = independent_session_cells(&events, offset, START_NS, END_NS, false);
        assert_eq!(
            got["session_cells"], local,
            "the summary must be segment-local"
        );
        assert_ne!(
            local, global,
            "the fixture must distinguish segment-local from global as-of"
        );
    }

    #[test]
    fn summary_matches_an_independent_tick_walk() {
        // Two implementations of one contract police each other: `summarize`
        // streams, this accumulation collects the identical seeded walk into
        // a Vec (quotes included - the CSV trade output omits them and could
        // not verify this) and recomputes every field with index-based scans.
        const WINDOW_NS: u64 = 20 * 60 * 1_000_000_000;
        let profile = resolve_profile("MNQ").expect("MNQ profile");
        let build = || {
            mogwai_data::GeneratedSource::try_new_with_session_profile(
                profile.scalars.clone(),
                DEFAULT_GEN_SEED,
                0,
                fingerprint(),
                &profile.session,
                None,
                mogwai_data::SizeGrid::from_def(&profile.def),
                profile.calendar.clone(),
            )
            .expect("source")
        };

        let mut streaming = build();
        let acc = summarize(&mut streaming, &profile, DEFAULT_GEN_SEED, 0, WINDOW_NS);
        let got = serde_json::to_value(&acc).expect("summary value");

        // Independent walk: collect, then recompute.
        let mut collected = build();
        let mut events = Vec::new();
        while let Some(e) = collected.next_tick() {
            if e.ts_event() >= WINDOW_NS {
                break;
            }
            events.push(e);
        }
        let tick = f64::try_from(profile.scalars.modal_tick).expect("tick");
        let cal = profile.calendar.as_ref();
        let open_all = |t1: u64, t2: u64| -> bool {
            let Some(c) = cal else { return true };
            let mut t = t1;
            loop {
                if !c.is_open(t) {
                    return false;
                }
                if t >= t2 {
                    return true;
                }
                t = (t + 1_000_000_000).min(t2);
            }
        };

        // Split into (quote, child-trades) chunks by index.
        struct Chunk {
            bid: Decimal,
            ask: Decimal,
            bid_sz: Decimal,
            ask_sz: Decimal,
            trades: Vec<TradeTick>,
        }
        let mut chunks: Vec<Chunk> = Vec::new();
        for e in &events {
            match e {
                TickEvent::Quote(q) => chunks.push(Chunk {
                    bid: q.bid_px,
                    ask: q.ask_px,
                    bid_sz: q.bid_sz,
                    ask_sz: q.ask_sz,
                    trades: Vec::new(),
                }),
                TickEvent::Trade(t) => {
                    if let Some(c) = chunks.last_mut() {
                        c.trades.push(t.clone());
                    }
                }
            }
        }
        let parents: Vec<&Chunk> = chunks.iter().filter(|c| !c.trades.is_empty()).collect();

        let mut parents_n = 0u64;
        let mut sided = 0u64;
        let mut single = 0u64;
        let mut levels_sum = 0u64;
        let mut gap_sum = 0u64;
        let mut gaps = 0u64;
        let mut ret_count = 0u64;
        let mut ret_sum = 0f64;
        let mut ret_sumsq = 0f64;
        let mut prev: Option<(u64, f64)> = None;
        for c in &parents {
            let first = &c.trades[0];
            parents_n += 1;
            sided += c.trades.len() as u64;
            if c.trades.len() == 1 {
                single += 1;
            }
            let mut lv: Vec<Decimal> = Vec::new();
            for t in &c.trades {
                if !lv.contains(&t.price) {
                    lv.push(t.price);
                }
            }
            levels_sum += lv.len() as u64;
            let mid = (f64::try_from(c.bid).unwrap() + f64::try_from(c.ask).unwrap()) / 2.0;
            if let Some((pt, pm)) = prev
                && open_all(pt, first.ts_event)
            {
                gap_sum += first.ts_event - pt;
                gaps += 1;
                let r = (mid / pm).ln();
                ret_count += 1;
                ret_sum += r;
                ret_sumsq += r * r;
            }
            prev = Some((first.ts_event, mid));
        }
        assert_eq!(got["parents"], serde_json::json!(parents_n));
        assert_eq!(got["sided_rows"], serde_json::json!(sided));
        assert_eq!(got["single_parents"], serde_json::json!(single));
        assert_eq!(got["level_count_sum"], serde_json::json!(levels_sum));
        assert_eq!(got["gap_sum_ns"], serde_json::json!(gap_sum));
        assert_eq!(got["eligible_gaps"], serde_json::json!(gaps));
        // EXACT equality on the float moments: both accumulators traverse
        // identical values in identical order, so their sums are bitwise
        // equal - a tolerance here would hide an ordering divergence.
        assert_eq!(got["mid_return_count"], serde_json::json!(ret_count));
        let got_sum = got["mid_return_sum"].as_f64().unwrap();
        let got_sumsq = got["mid_return_sumsq"].as_f64().unwrap();
        assert_eq!(
            got_sum.to_bits(),
            ret_sum.to_bits(),
            "{got_sum} vs {ret_sum}"
        );
        assert_eq!(
            got_sumsq.to_bits(),
            ret_sumsq.to_bits(),
            "{got_sumsq} vs {ret_sumsq}"
        );

        // Histograms, recomputed independently.
        let mut size_hist = std::collections::BTreeMap::new();
        for e in &events {
            if let TickEvent::Trade(t) = e {
                *size_hist
                    .entry(t.size.normalize().to_string())
                    .or_insert(0u64) += 1;
            }
        }
        assert_eq!(
            got["size_histogram"],
            serde_json::to_value(&size_hist).unwrap()
        );
        let mut width_hist = std::collections::BTreeMap::new();
        let mut bid_hist = std::collections::BTreeMap::new();
        let mut ask_hist = std::collections::BTreeMap::new();
        let mut buyer_hist = std::collections::BTreeMap::new();
        let mut seller_hist = std::collections::BTreeMap::new();
        for c in &parents {
            let w: u64 = ((c.ask - c.bid) / profile.scalars.modal_tick)
                .round()
                .try_into()
                .unwrap();
            *width_hist.entry(w.to_string()).or_insert(0u64) += 1;
            *bid_hist
                .entry(c.bid_sz.normalize().to_string())
                .or_insert(0u64) += 1;
            *ask_hist
                .entry(c.ask_sz.normalize().to_string())
                .or_insert(0u64) += 1;
            let first = &c.trades[0];
            let mid = (f64::try_from(c.bid).unwrap() + f64::try_from(c.ask).unwrap()) / 2.0;
            let raw = (f64::try_from(first.price).unwrap() - mid) / tick;
            let signed = match first.aggressor {
                AggressorSide::Buyer => Some((raw, &mut buyer_hist)),
                AggressorSide::Seller => Some((-raw, &mut seller_hist)),
                AggressorSide::NoAggressor => None,
            };
            if let Some((d, hist)) = signed {
                let bin = (d / DISPLACEMENT_BIN_TICKS).floor() * DISPLACEMENT_BIN_TICKS;
                *hist.entry(format!("{bin:.2}")).or_insert(0u64) += 1;
            }
        }
        // width keys are u64 in the summary; compare through string maps.
        let got_width: std::collections::BTreeMap<String, u64> =
            serde_json::from_value(got["width_ticks_histogram"].clone()).unwrap();
        assert_eq!(got_width, width_hist);
        assert_eq!(
            got["bid_size_histogram"],
            serde_json::to_value(&bid_hist).unwrap()
        );
        assert_eq!(
            got["ask_size_histogram"],
            serde_json::to_value(&ask_hist).unwrap()
        );
        assert_eq!(
            got["buyer_displacement_hist"],
            serde_json::to_value(&buyer_hist).unwrap()
        );
        assert_eq!(
            got["seller_displacement_hist"],
            serde_json::to_value(&seller_hist).unwrap()
        );

        // Fixed-horizon diagnostics: as-of mids at every boundary, linear
        // scans over the collected quotes.
        let quotes: Vec<(u64, f64)> = events
            .iter()
            .filter_map(|e| match e {
                TickEvent::Quote(q) => Some((
                    q.ts_event,
                    (f64::try_from(q.bid_px).unwrap() + f64::try_from(q.ask_px).unwrap()) / 2.0,
                )),
                TickEvent::Trade(_) => None,
            })
            .collect();
        for horizon_s in SUMMARY_VOL_HORIZONS_S {
            let h_ns = horizon_s * 1_000_000_000;
            let mut count = 0u64;
            let mut sum = 0f64;
            let mut sumsq = 0f64;
            // Every boundary at or before `end` accumulates: in-window quotes
            // settle the early ones and the end-of-walk flush settles the
            // rest, INCLUDING the boundary sitting exactly on
            // measured_until_ns - both horizons here divide the window, so
            // that exact case is exercised for each.
            assert_eq!(WINDOW_NS % h_ns, 0, "window must exercise the end boundary");
            let mut k = 1u64;
            while k * h_ns <= WINDOW_NS {
                let boundary = k * h_ns;
                let asof = |b: u64| {
                    quotes
                        .iter()
                        .rev()
                        .find(|(ts, _)| *ts <= b)
                        .map(|(_, m)| *m)
                };
                if let (Some(prev_m), Some(cur_m)) = (asof(boundary - h_ns), asof(boundary))
                    && prev_m > 0.0
                    && cur_m > 0.0
                    && open_all(boundary - h_ns, boundary)
                {
                    let r = (cur_m / prev_m).ln();
                    count += 1;
                    sum += r;
                    sumsq += r * r;
                }
                k += 1;
            }
            let got_h = &got["horizon_vol"][horizon_s.to_string()];
            assert_eq!(got_h["count"], serde_json::json!(count), "h={horizon_s}");
            let gs = got_h["sum"].as_f64().unwrap();
            let gq = got_h["sumsq"].as_f64().unwrap();
            assert_eq!(gs.to_bits(), sum.to_bits(), "h={horizon_s}: {gs} vs {sum}");
            assert_eq!(
                gq.to_bits(),
                sumsq.to_bits(),
                "h={horizon_s}: {gq} vs {sumsq}"
            );
        }

        // -- Protocol-11 session cells (spec 4.5-4.6), independently -------
        // The session-date arithmetic itself is pinned against hand-computed
        // instants first, because both sides share session_segment_at and a
        // defect there would otherwise cancel out. 2026-07-07T15:00:00Z is
        // 10:00 Tuesday local under -300: overnight segment of the session
        // opened Monday 17:00 local (2026-07-06T22:00:00Z), ending Tuesday
        // 16:00 local (2026-07-07T21:00:00Z), halt at 20:15Z.
        let seg = session_segment_at(1_783_436_400_000_000_000, -300).expect("open instant");
        assert_eq!(
            seg.session_start_ns,
            1_783_461_600_000_000_000 - 86_400_000_000_000
        );
        assert_eq!(seg.session_end_ns, 1_783_458_000_000_000_000);
        assert_eq!(seg.segment_origin_ns, seg.session_start_ns);
        assert_eq!(seg.segment_end_ns, 1_783_455_300_000_000_000);
        // 15:40 local the same Tuesday: the post-halt segment.
        let post = session_segment_at(1_783_456_800_000_000_000, -300).expect("post-halt");
        assert_eq!(post.session_start_ns, seg.session_start_ns);
        assert_eq!(post.segment_origin_ns, 1_783_456_200_000_000_000);
        assert_eq!(post.segment_end_ns, seg.session_end_ns);
        // 15:20 local: inside the halt, closed.
        assert!(session_segment_at(1_783_455_600_000_000_000, -300).is_none());

        let offset = profile
            .calendar
            .as_ref()
            .expect("MNQ calendar")
            .utc_offset_minutes;
        let expected = independent_session_cells(&events, offset, 0, WINDOW_NS, true);
        assert_eq!(got["session_cells"], expected);
        // The under-capacity top-minute case: a 20-minute walk populates
        // fewer minutes than the capacity, and every populated minute
        // appears as a record.
        let populated = got["minute_range_ticks_hist"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_u64().unwrap())
            .sum::<u64>();
        assert!(populated as usize <= TOP_MINUTE_RECORDS);
        assert_eq!(
            got["top_minutes"].as_array().unwrap().len() as u64,
            populated
        );

        // The frozen top-minute ranking edge cases (spec 4.7), on crafted
        // maps rather than incidental stream content: empty, repeated
        // maxima as distinct entries, and equal-range ties ordered by
        // earlier minute. (The under-capacity and over-capacity cases are
        // the organic assertions in this test and the bar-pass test.)
        let tick = profile.scalars.modal_tick;
        let empty: std::collections::BTreeMap<u64, (Decimal, Decimal, u64)> = Default::default();
        assert!(rank_top_minutes(&empty, &Default::default(), tick).is_empty());
        let base = Decimal::try_from(23_000.0).expect("base price");
        let two_ticks = tick + tick;
        let mut crafted: std::collections::BTreeMap<u64, (Decimal, Decimal, u64)> =
            Default::default();
        crafted.insert(7, (base, base + two_ticks, 3)); // range 2, later
        crafted.insert(5, (base, base + two_ticks, 4)); // range 2, earlier
        crafted.insert(9, (base, base + tick, 1)); // range 1
        let ranked = rank_top_minutes(&crafted, &Default::default(), tick);
        assert_eq!(
            ranked.iter().map(|r| r.minute_start_ns).collect::<Vec<_>>(),
            vec![5 * 60_000_000_000, 7 * 60_000_000_000, 9 * 60_000_000_000],
            "repeated maxima occupy distinct entries and ties order by \
             earlier minute"
        );
        assert_eq!(ranked[0].range_ticks, 2);
        assert_eq!(ranked[1].range_ticks, 2);

        // A calendar-free profile has no sessions by construction: the
        // session-cell vector must be empty, never a fabricated 24/7 week.
        let crypto = resolve_profile("BTCUSDT").expect("BTCUSDT profile");
        let mut crypto_source = mogwai_data::GeneratedSource::try_new_with_session_profile(
            crypto.scalars.clone(),
            DEFAULT_GEN_SEED,
            0,
            fingerprint(),
            &crypto.session,
            None,
            mogwai_data::SizeGrid::from_def(&crypto.def),
            crypto.calendar.clone(),
        )
        .expect("crypto source");
        let crypto_acc = summarize(
            &mut crypto_source,
            &crypto,
            DEFAULT_GEN_SEED,
            0,
            5 * 60 * 1_000_000_000,
        );
        let crypto_got = serde_json::to_value(&crypto_acc).expect("crypto summary");
        assert_eq!(crypto_got["session_cells"], serde_json::json!([]));
    }

    /// Phase 1 (the retired rewrite plan) unifies the session/segment
    /// math in `mogwai-lab`, but does NOT rewire this crate onto it yet
    /// (phase 2). This test pins that the two implementations agree so the
    /// eventual rewire is behavior-preserving by construction: any
    /// divergence introduced before phase 2 fails here immediately instead
    /// of surfacing as a silent generator drift later.
    #[test]
    fn session_segment_at_agrees_with_mogwai_lab() {
        let offset: i16 = -300;
        // A dense sweep across several days at 1-minute resolution covers
        // every branch (evening-overnight, morning-overnight, the halt gap,
        // post_halt, and the daily break) many times over.
        let start_day_ns: u64 = 20_635 * 86_400 * 1_000_000_000; // 2026-07-01
        for minute in 0..(3 * 24 * 60) {
            let ts = start_day_ns + minute * 60_000_000_000;
            let got = session_segment_at(ts, offset);
            let want = mogwai_lab::session::session_segment_at(ts, i32::from(offset));
            match (got, want) {
                (None, None) => {}
                (Some(g), Some(w)) => {
                    assert_eq!(g.session_start_ns, w.session_start_ns, "ts={ts}");
                    assert_eq!(g.session_end_ns, w.session_end_ns, "ts={ts}");
                    assert_eq!(g.segment_origin_ns, w.segment_origin_ns, "ts={ts}");
                    assert_eq!(g.segment_end_ns, w.segment_end_ns, "ts={ts}");
                }
                (g, w) => panic!(
                    "disagreement at ts={ts}: gen.rs={g:?} mogwai-lab present={}",
                    w.is_some()
                ),
            }
        }
    }
}
