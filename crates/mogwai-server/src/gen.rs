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

use crate::source::{InstrumentProfiles, fingerprint};

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
    /// preset name (MNQ, MES, BTCUSDT, ETHUSDT, SOLUSDT). A preset brings its
    /// own session calendar, so a futures tape shows its closed weekend.
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    /// Resolve the instrument from an operator config TOML instead of a
    /// symbol, through the server's REAL `Config::load` and profile
    /// construction - the same path a served config takes - so a scratch
    /// profile with candidate scalars is expressible without touching
    /// committed presets. The file must carry exactly one `[instrument]`.
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
        (GenType::Trades | GenType::Summary, Some(_)) => {
            bail!("--interval is only valid with --type bars")
        }
        (GenType::Trades | GenType::Summary, None) => None,
    };
    if args.warmup.is_some() && !matches!(args.kind, GenType::Summary) {
        bail!("--warmup is only valid with --type summary");
    }

    let profile = resolve_profile_for(args)?;

    if let GenType::Summary = args.kind {
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
        let mut source = build_source(args, &profile, walk_start)?;
        let acc = summarize(
            &mut source,
            &profile,
            args.seed.unwrap_or(DEFAULT_GEN_SEED),
            args.start,
            end,
        );
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
        GenType::Summary => unreachable!("summary dispatched above"),
    }
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
    profile: &crate::source::InstrumentProfile,
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
fn resolve_profile_for(args: &GenArgs) -> anyhow::Result<crate::source::InstrumentProfile> {
    if let Some(path) = &args.config {
        return profile_from_config(path);
    }
    resolve_profile(&args.symbol)
}

/// One instrument profile from an operator config file, through the SAME
/// `Config::load` and instrument-profile construction a served run boots
/// with, so a scratch config exercises exactly the shipped validation and
/// defaulting. The config must configure an instrument: the built-in default
/// venue would silently ignore every scratch scalar.
fn profile_from_config(path: &std::path::Path) -> anyhow::Result<crate::source::InstrumentProfile> {
    let cfg = crate::config::Config::load(Some(path.to_path_buf()))
        .with_context(|| format!("loading --config {}", path.display()))?;
    if cfg.instrument.is_none() {
        bail!(
            "--config {} carries no [instrument] table; a scratch profile must configure one",
            path.display()
        );
    }
    let profiles = crate::config::build_instrument_profiles(&cfg)?;
    let defs = profiles.instrument_defs();
    let [def] = defs.as_slice() else {
        bail!(
            "--config resolved {} instruments, expected exactly one",
            defs.len()
        );
    };
    Ok(profiles
        .get(&def.symbol)
        .expect("just listed this symbol")
        .clone())
}

/// A named symbol: a built-in venue symbol first, then an embedded preset.
/// Checking the venue first keeps `--symbol BTCUSDT` byte-identical to what
/// it produced before presets were reachable here.
fn resolve_profile(symbol: &str) -> anyhow::Result<crate::source::InstrumentProfile> {
    if let Some(profile) = InstrumentProfiles::defaults().get(symbol) {
        return Ok(profile.clone());
    }
    crate::config::profile_from_preset(symbol).with_context(|| {
        format!("unknown symbol {symbol}: not a built-in venue symbol and not an embedded preset")
    })
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

// ---------------------------------------------------------------------------
// Summary mode: the MNQ TBBO fit's calibration instrument. One JSON object of
// BOUNDED sufficient statistics per run (one seed per invocation; the harness
// pools seeds). Every distributional field is a histogram or a
// count/sum/sum-of-squares accumulator, never a raw array - a simulated month
// is order 10^7 parents. Consumes the full `next_tick()` walk: every draw
// (sizes, prices, sides, quotes) is materialized, which `advance_parent()`'s
// compact summary deliberately is not.
// ---------------------------------------------------------------------------

/// Signed-displacement bin width in ticks. Wrong-side observations land in
/// negative bins.
const DISPLACEMENT_BIN_TICKS: f64 = 0.05;

/// Fixed horizons (seconds) for the secondary realized-vol diagnostics.
const SUMMARY_VOL_HORIZONS_S: [u64; 2] = [60, 300];

const NS_PER_MINUTE: u64 = 60 * 1_000_000_000;

#[derive(Default, serde::Serialize)]
struct MomentAcc {
    count: u64,
    sum: f64,
    sumsq: f64,
}

impl MomentAcc {
    fn push(&mut self, x: f64) {
        self.count += 1;
        self.sum += x;
        self.sumsq += x * x;
    }
}

#[derive(serde::Serialize)]
pub(crate) struct SummaryAcc {
    seed: u64,
    parents: u64,
    sided_rows: u64,
    single_parents: u64,
    level_count_sum: u64,
    gap_sum_ns: u64,
    eligible_gaps: u64,
    size_histogram: std::collections::BTreeMap<String, u64>,
    bid_size_histogram: std::collections::BTreeMap<String, u64>,
    ask_size_histogram: std::collections::BTreeMap<String, u64>,
    width_ticks_histogram: std::collections::BTreeMap<u64, u64>,
    buyer_displacement_hist: std::collections::BTreeMap<String, u64>,
    seller_displacement_hist: std::collections::BTreeMap<String, u64>,
    mid_return_count: u64,
    mid_return_sum: f64,
    mid_return_sumsq: f64,
    horizon_vol: std::collections::BTreeMap<String, MomentAcc>,
    first_book_mid: Option<String>,
    measured_from_ns: u64,
    measured_until_ns: u64,
}

/// True when the calendar is open across the whole of `[t1, t2]`. Closure
/// boundaries sit on calendar minutes, so checking both endpoints and every
/// minute boundary between them is exact. No calendar means always open.
fn open_throughout(calendar: Option<&mogwai_data::SessionCalendar>, t1: u64, t2: u64) -> bool {
    let Some(cal) = calendar else { return true };
    if !cal.is_open(t1) || !cal.is_open(t2) {
        return false;
    }
    let mut t = (t1 / NS_PER_MINUTE + 1) * NS_PER_MINUTE;
    while t < t2 {
        if !cal.is_open(t) {
            return false;
        }
        t += NS_PER_MINUTE;
    }
    true
}

fn decimal_key(d: Decimal) -> String {
    d.normalize().to_string()
}

fn displacement_key(d_ticks: f64) -> String {
    let bin = (d_ticks / DISPLACEMENT_BIN_TICKS).floor() * DISPLACEMENT_BIN_TICKS;
    format!("{bin:.2}")
}

/// One inferred parent in flight: the quote that preceded it and what its
/// children have printed so far. Parents are delimited by quote emissions -
/// protocol 7 publishes exactly one book before every parent burst.
struct OpenParent {
    quote_mid: f64,
    width_ticks: u64,
    bid_sz: Decimal,
    ask_sz: Decimal,
    first_ts: u64,
    first_price: Decimal,
    first_side: AggressorSide,
    rows: u64,
    levels: Vec<Decimal>,
}

/// Fold the tick stream into the summary. Accumulation covers exactly
/// `[start, end)` by each parent's FIRST child timestamp; a warm-up walk
/// before `start` is consumed and discarded. The source must already sit at
/// its walk start (possibly `start - warmup`).
pub(crate) fn summarize(
    source: &mut dyn TickSource,
    profile: &crate::source::InstrumentProfile,
    seed: u64,
    start: u64,
    end: u64,
) -> SummaryAcc {
    let tick = profile.scalars.modal_tick;
    let tick_f = f64::try_from(tick).unwrap_or(f64::NAN);
    let calendar = profile.calendar.as_ref();

    let mut acc = SummaryAcc {
        seed,
        parents: 0,
        sided_rows: 0,
        single_parents: 0,
        level_count_sum: 0,
        gap_sum_ns: 0,
        eligible_gaps: 0,
        size_histogram: Default::default(),
        bid_size_histogram: Default::default(),
        ask_size_histogram: Default::default(),
        width_ticks_histogram: Default::default(),
        buyer_displacement_hist: Default::default(),
        seller_displacement_hist: Default::default(),
        mid_return_count: 0,
        mid_return_sum: 0.0,
        mid_return_sumsq: 0.0,
        horizon_vol: SUMMARY_VOL_HORIZONS_S
            .iter()
            .map(|h| (h.to_string(), MomentAcc::default()))
            .collect(),
        first_book_mid: None,
        measured_from_ns: start,
        measured_until_ns: end,
    };

    // As-of state for the fixed-horizon diagnostics: per horizon, the index of
    // the next boundary and the as-of mid at the previous boundary.
    let mut horizon_state: Vec<(u64, u64, Option<f64>)> = SUMMARY_VOL_HORIZONS_S
        .iter()
        .map(|h| (h * 1_000_000_000, 1, None))
        .collect();
    let mut last_mid: Option<f64> = None;
    // The as-of mid AT `start`, frozen when the first post-start quote
    // arrives: the first boundary's window opens at `start`, and a warm-up
    // quote at or before it is its legitimate as-of observation. The flag
    // marks the freeze, because the frozen VALUE is legitimately None when
    // no quote precedes `start` - an Option's is_none cannot distinguish
    // not-yet-frozen from frozen-empty and would re-freeze every quote.
    let mut asof_start: Option<f64> = None;
    let mut asof_start_frozen = false;

    let mut pending_quote: Option<(f64, u64, Decimal, Decimal)> = None;
    let mut open: Option<OpenParent> = None;
    let mut prev_parent: Option<(u64, f64)> = None; // (first_ts, quote_mid)

    let finalize = |acc: &mut SummaryAcc, prev: &mut Option<(u64, f64)>, parent: OpenParent| {
        if parent.first_ts < start || parent.first_ts >= end {
            return;
        }
        acc.parents += 1;
        acc.sided_rows += parent.rows;
        if parent.rows == 1 {
            acc.single_parents += 1;
        }
        acc.level_count_sum += parent.levels.len() as u64;
        *acc.width_ticks_histogram
            .entry(parent.width_ticks)
            .or_insert(0) += 1;
        *acc.bid_size_histogram
            .entry(decimal_key(parent.bid_sz))
            .or_insert(0) += 1;
        *acc.ask_size_histogram
            .entry(decimal_key(parent.ask_sz))
            .or_insert(0) += 1;
        if parent.quote_mid.is_finite() && tick_f.is_finite() {
            let first = f64::try_from(parent.first_price).unwrap_or(f64::NAN);
            let raw = (first - parent.quote_mid) / tick_f;
            match parent.first_side {
                AggressorSide::Buyer => {
                    *acc.buyer_displacement_hist
                        .entry(displacement_key(raw))
                        .or_insert(0) += 1;
                }
                AggressorSide::Seller => {
                    *acc.seller_displacement_hist
                        .entry(displacement_key(-raw))
                        .or_insert(0) += 1;
                }
                AggressorSide::NoAggressor => {}
            }
        }
        if let Some((prev_ts, prev_mid)) = *prev
            && open_throughout(calendar, prev_ts, parent.first_ts)
        {
            acc.gap_sum_ns += parent.first_ts.saturating_sub(prev_ts);
            acc.eligible_gaps += 1;
            if prev_mid > 0.0 && parent.quote_mid > 0.0 {
                let r = (parent.quote_mid / prev_mid).ln();
                acc.mid_return_count += 1;
                acc.mid_return_sum += r;
                acc.mid_return_sumsq += r * r;
            }
        }
        *prev = Some((parent.first_ts, parent.quote_mid));
    };

    while let Some(event) = source.next_tick() {
        let ts = event.ts_event();
        if ts >= end {
            break;
        }
        match event {
            TickEvent::Quote(q) => {
                // A quote closes the parent that ran under the PREVIOUS book.
                if let Some(parent) = open.take() {
                    finalize(&mut acc, &mut prev_parent, parent);
                }
                let bid = f64::try_from(q.bid_px).unwrap_or(f64::NAN);
                let ask = f64::try_from(q.ask_px).unwrap_or(f64::NAN);
                let mid = (bid + ask) / 2.0;
                let width = ((q.ask_px - q.bid_px) / tick)
                    .round()
                    .try_into()
                    .unwrap_or(u64::MAX);
                pending_quote = Some((mid, width, q.bid_sz, q.ask_sz));
                if ts >= start && acc.first_book_mid.is_none() {
                    acc.first_book_mid = Some(decimal_key((q.bid_px + q.ask_px) / Decimal::TWO));
                }
                // The fixed-horizon as-of state advances on quotes only: the
                // mid IS the quote mid, and a boundary takes the last mid at
                // or before it.
                if ts > start && !asof_start_frozen {
                    asof_start = last_mid;
                    asof_start_frozen = true;
                }
                for (h_ns, next_k, prev_boundary_mid) in &mut horizon_state {
                    let horizon = *h_ns;
                    loop {
                        let boundary = start.saturating_add(*next_k * horizon);
                        if ts <= boundary {
                            break;
                        }
                        let window_start = boundary - horizon;
                        let prev = prev_boundary_mid.or(asof_start);
                        if let (Some(prev), Some(cur)) = (prev, last_mid)
                            && prev > 0.0
                            && cur > 0.0
                            && open_throughout(calendar, window_start, boundary)
                        {
                            let key = (horizon / 1_000_000_000).to_string();
                            if let Some(m) = acc.horizon_vol.get_mut(&key) {
                                m.push((cur / prev).ln());
                            }
                        }
                        *prev_boundary_mid = last_mid;
                        *next_k += 1;
                    }
                }
                last_mid = Some(mid);
            }
            TickEvent::Trade(t) => {
                if t.ts_event >= start && t.ts_event < end {
                    *acc.size_histogram.entry(decimal_key(t.size)).or_insert(0) += 1;
                }
                match &mut open {
                    Some(parent) => {
                        parent.rows += 1;
                        if !parent.levels.contains(&t.price) {
                            parent.levels.push(t.price);
                        }
                    }
                    None => {
                        let Some((mid, width, bid_sz, ask_sz)) = pending_quote else {
                            continue; // pre-first-quote trade: no book to attribute
                        };
                        open = Some(OpenParent {
                            quote_mid: mid,
                            width_ticks: width,
                            bid_sz,
                            ask_sz,
                            first_ts: t.ts_event,
                            first_price: t.price,
                            first_side: t.aggressor,
                            rows: 1,
                            levels: vec![t.price],
                        });
                    }
                }
            }
        }
    }
    if let Some(parent) = open.take() {
        finalize(&mut acc, &mut prev_parent, parent);
    }
    // Flush the fixed-horizon boundaries at or before `end` that no in-window
    // quote arrived strictly after: their as-of mid is the final `last_mid`,
    // since the walk produced no further quotes inside the window. Without
    // this the last window of the measurement - including one whose boundary
    // sits exactly on `measured_until_ns` - is silently dropped. If no quote
    // ever arrived after `start`, the freeze never ran and the walk-long
    // as-of at `start` is the final mid too.
    if !asof_start_frozen {
        asof_start = last_mid;
    }
    for (h_ns, next_k, prev_boundary_mid) in &mut horizon_state {
        let horizon = *h_ns;
        loop {
            let boundary = start.saturating_add(*next_k * horizon);
            if boundary > end {
                break;
            }
            let window_start = boundary - horizon;
            let prev = prev_boundary_mid.or(asof_start);
            if let (Some(prev), Some(cur)) = (prev, last_mid)
                && prev > 0.0
                && cur > 0.0
                && open_throughout(calendar, window_start, boundary)
            {
                let key = (horizon / 1_000_000_000).to_string();
                if let Some(m) = acc.horizon_vol.get_mut(&key) {
                    m.push((cur / prev).ln());
                }
            }
            *prev_boundary_mid = last_mid;
            *next_k += 1;
        }
    }
    acc
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
            regime: None,
            havoc: None,
            out: None,
        };
        let profile = resolve_profile_for(&args).expect("cli profile");
        let mut cli_source = build_source(&args, &profile, args.start).expect("cli source");

        let fp = fingerprint();
        let profiles = InstrumentProfiles::defaults();
        let profile = profiles.get("BTCUSDT").expect("BTCUSDT profile");
        let mut direct_source = mogwai_data::GeneratedSource::new_with_session_profile(
            profile.scalars.clone(),
            DEFAULT_GEN_SEED,
            0,
            fp,
            &profile.session,
            None,
            mogwai_data::SizeGrid::spot(),
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
        assert_eq!(profile.def.symbol, "MNQ");
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
    fn an_unknown_symbol_names_both_places_it_was_looked_for() {
        let err = resolve_profile("NOPE").expect_err("NOPE is neither venue nor preset");
        let text = format!("{err}");
        assert!(
            text.contains("NOPE"),
            "message should name the symbol: {text}"
        );
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
    // Brick G of notes/mnq-tbbo-fit-spec.md: the calibration instrument.
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

    #[test]
    fn a_scratch_config_profile_matches_the_served_profile() {
        // The whole point of --config is that a scratch profile takes the
        // SAME loading path a served config takes. A config that merely
        // names the MNQ preset must therefore resolve to exactly the profile
        // the embedded preset resolves to.
        let path = scratch_config("plain-mnq.toml", "[instrument]\npreset = \"MNQ\"\n");
        let from_config = profile_from_config(&path).expect("config profile");
        let from_preset = crate::config::profile_from_preset("MNQ").expect("preset profile");
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
        let mnq =
            |over: &str| format!("[instrument]\npreset = \"MNQ\"\n[instrument.override]\n{over}\n");
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
            ("levels_mean", "\"generator.levels_mean\" = 1.2"),
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
            regime: None,
            havoc: None,
            out: None,
        };
        let mut sink = Vec::new();
        let err = run_into(&args, &mut sink).expect_err("underflowing warmup must refuse");
        let text = format!("{err:#}");
        assert!(text.contains("underflows"), "names the underflow: {text}");
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
    }
}
