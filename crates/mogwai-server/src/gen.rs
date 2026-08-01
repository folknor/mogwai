// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai gen` - runs the synthetic generator OFFLINE (no server, no sockets,
//! no adapter) and writes its output as CSV, either raw trades or aggregated
//! OHLCV bars, so the generated tape can be charted and inspected. Reuses the
//! server's own generation plumbing (`InstrumentProfiles`, `seed_for`,
//! `fingerprint()`, `GeneratedSource`) and the shared bar-aggregation core
//! (`mogwai_data::{BarAcc, fold_trade}`) so this CLI never diverges from the
//! walk the running server would produce at the same anchor.
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

use crate::source::{InstrumentProfiles, fingerprint, seed_for};

#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum GenType {
    Trades,
    Bars,
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
    /// Instrument to generate. Selects a built-in fingerprint profile.
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    /// Walk seed. Defaults to the FNV of `--symbol`, matching the running server.
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
    /// regime. The whole spec is validated (a file broadarrow would reject is
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
        (GenType::Trades, Some(_)) => bail!("--interval is not valid with --type trades"),
        (GenType::Trades, None) => None,
    };

    let mut source = build_source(args)?;
    let start = args.start;
    let trades = std::iter::from_fn(move || match source.next_tick() {
        Some(TickEvent::Trade(t)) => Some(t),
        _ => None,
    })
    .take_while(move |t| t.ts_event < end);

    match args.kind {
        GenType::Trades => write_trades(trades.filter(move |t| t.ts_event >= start), sink)?,
        GenType::Bars => {
            let interval = interval.expect("bars validated interval above");
            write_bars(trades, args.start, end, interval, sink)?;
        }
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

/// Resolve profile + seed + start-price override + regime and build the walk via
/// the fallible constructor (a bad `--start-price` is an error, not a panic).
fn build_source(args: &GenArgs) -> anyhow::Result<mogwai_data::GeneratedSource> {
    let fp = fingerprint();
    let profiles = InstrumentProfiles::defaults();
    let profile = profiles.get(&args.symbol).with_context(|| {
        format!(
            "unknown symbol {}: the built-in venue does not list it",
            args.symbol
        )
    })?;
    let seed = args.seed.unwrap_or_else(|| seed_for(&args.symbol));
    let mut scalars = profile.scalars.clone();
    if let Some(p) = args.start_price {
        scalars.start_price = p;
    }
    let regime = resolve_regime(args)?;

    mogwai_data::GeneratedSource::try_new_with_session_profile(
        scalars,
        seed,
        args.start,
        fp,
        &profile.session,
        regime,
    )
    .map_err(|e| anyhow::anyhow!("building the generator: {e:?}"))
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
            regime: None,
            havoc: None,
            out: None,
        };
        let mut cli_source = build_source(&args).expect("cli source");

        let fp = fingerprint();
        let profiles = InstrumentProfiles::defaults();
        let profile = profiles.get("BTCUSDT").expect("BTCUSDT profile");
        let mut direct_source = mogwai_data::GeneratedSource::new_with_session_profile(
            profile.scalars.clone(),
            seed_for("BTCUSDT"),
            0,
            fp,
            &profile.session,
            None,
        );

        for _ in 0..50 {
            let cli_tick = cli_source.next_tick().expect("cli tick");
            let direct_tick = direct_source.next_tick().expect("direct tick");
            let TickEvent::Trade(cli_trade) = cli_tick else {
                panic!("generator emits trades");
            };
            let TickEvent::Trade(direct_trade) = direct_tick else {
                panic!("generator emits trades");
            };
            assert_eq!(cli_trade.ts_event, direct_trade.ts_event);
            assert_eq!(cli_trade.price, direct_trade.price);
            assert_eq!(cli_trade.size, direct_trade.size);
        }
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
}
