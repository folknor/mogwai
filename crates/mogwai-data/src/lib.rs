// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Tick storage + replay. A [`TickSource`] yields ticks in time order and the
//! server fans them out to subscribers as market data; a [`Permutation`] can
//! transform them in flight (e.g. tick-rule aggressor inference).
//!
//! Two source lineages implement [`TickSource`], and they are not
//! interchangeable - the running server only ever uses the first:
//!
//! - [`GeneratedSource`] - the synthetic generator the RUNNING server uses. It
//!   synthesizes ticks from a committed fingerprint fitted offline to Kraken
//!   trade history; it opens no file, is a pure path-dependent walk (same seed
//!   plus tape anchor yields the same stream byte for byte), and is effectively
//!   infinite. [`CheckpointIndex`] turns a from-origin seek over it from
//!   O(distance) into O(K) by snapshotting the walk every K ticks and replaying
//!   only the residual.
//! - [`KrakenCsvSource`] - the offline-analysis lineage, retained for fitting
//!   the fingerprint rather than for serving. Reads the Kraken trade-history CSV
//!   dump: one file per pair (symbol taken from the file stem), no header, three
//!   columns `time,price,volume` where time is unix **seconds** (optionally
//!   fractional). Files reach multiple GB, so it streams one buffered line at a
//!   time - O(1) memory.
//!
//! [`MergeSource`] k-way merges several single-symbol sources into one
//! time-ordered stream, and [`MemorySource`] backs tests and the wiring skeleton.
//!
//! [`scan_triggers`] and [`vol_reading`] are the two bounded tape walks behind
//! the venue's fill band - one asks whether a print went through a drawn
//! trigger, the other reads the trailing realized volatility that sized the band
//! the trigger was drawn from. They live here, next to the sources they drain,
//! so the server and the benchmarks call the same shipped code rather than two
//! copies of it.

mod bars;
mod generated;
mod trigger;

use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    path::Path,
    str::FromStr,
};

use mogwai_protocol::{AggressorSide, QuoteTick, TradeTick};
use rust_decimal::Decimal;

pub use bars::{BarAcc, fold_trade, window_close_ns};
pub use generated::{
    ARRIVAL_KERNEL_VERSION, ARRIVAL_X_CEILING, AbsReturnAcf, AnchorRange, ArrivalConfig,
    ArrivalEnv, ArrivalKernel, ArrivalRefusal, ArrivalState, CadenceParts, CadenceWalk,
    CalendarError, CalibrationProvenance, CheckpointIndex, EmpiricalRanges, Fingerprint,
    GeneratedSource, GeneratedSourceError, GeneratorScalars, GoldenTargets, LogOuParams,
    MinMedianMax, ParentDraw, ParentSummary, PendingReopen, PublishedBook, QuotedWidth,
    RuntimeModifiers, ScalarDiagnostic, ScalarError, SelfExcitingParams, SessionCalendar,
    SessionProfile, SessionProfileError, ShotNoiseParams, SizeGrid, SweepShape, TickTraversal,
    TopOfBookSizes, TradeDisplacement, VolTrace, WallMmppParams, WeeklyWindow, book_mid_ticks,
    place_book,
};
pub use mogwai_protocol::MarketRegime;
pub use trigger::{
    FILL_HORIZON_NS, MIN_VOL_SAMPLES, TriggerScan, VOL_WINDOW_NS, VolReading, Walk, scan_triggers,
    vol_reading,
};

/// Identity of the tape generation process, not of any one path. Two runs are
/// comparable only if their venues report the same value. `AGENTS.md` carries
/// the obligation to bump this for every tape-determinism change.
///
/// 12 is the protocol-12b arrival-frame calibration repair (integrated
/// families take the bare mean): it changes outputs for `(config, seed)`
/// pairs already expressible under 11, even though no shipped preset declares
/// the arrival seam. The eventual 12b mechanism landing takes 13.
pub const TAPE_PROTOCOL_VERSION: u32 = 12;

/// A terminal condition that ended a [`TickSource`] before ordinary
/// exhaustion.
///
/// Sources report faults only after they have become terminal.  The default
/// [`TickSource::fault`] implementation is deliberately empty so the existing
/// finite and replay sources retain their ordinary exhaustion semantics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TickFault {
    Arrival(ArrivalRefusal),
}

/// One replayable market-data event.
#[derive(Debug, Clone)]
pub enum TickEvent {
    Trade(TradeTick),
    Quote(QuoteTick),
}

impl TickEvent {
    /// Event timestamp (unix nanoseconds) used to order the replay stream.
    pub fn ts_event(&self) -> u64 {
        match self {
            TickEvent::Trade(t) => t.ts_event,
            TickEvent::Quote(q) => q.ts_event,
        }
    }
}

/// A source of ticks in replay (time) order.
pub trait TickSource {
    /// Next tick in replay order, or `None` at end of stream.
    fn next_tick(&mut self) -> Option<TickEvent>;

    /// The terminal fault that ended this source, if any.
    ///
    /// `None` means ordinary exhaustion.  Consumers must query this after a
    /// terminal `None` when exhaustion and failure have different outcomes.
    fn fault(&self) -> Option<TickFault> {
        None
    }

    /// Arm a simulated-time flow surge. Non-generated sources ignore it.
    fn arm_flow_surge(
        &mut self,
        _start_ns: u64,
        _duration_ms: u64,
        _rate_mult: f64,
        _children_mult: f64,
    ) {
    }

    /// Clear an armed flow surge. Non-generated sources ignore it.
    fn clear_flow_surge(&mut self) {}

    /// Advance past ticks before `start_ts`, returning the first tick in the
    /// window. The default drains one tick at a time and keeps O(1) memory.
    ///
    /// Unbounded: a source whose `next_tick` never returns `None` (e.g.
    /// [`GeneratedSource`]) spins forever if `start_ts` is unreachable or
    /// simply far in the future, holding whatever lock the caller took to get
    /// here. This default has no way to bound the walk without an API change
    /// that would ripple to every implementer, so callers driving an
    /// effectively-infinite source must wrap it with their own bound (the
    /// server's checkpointed seek does this) rather than call `seek_to`
    /// directly against an untrusted target.
    fn seek_to(&mut self, start_ts: u64) -> Option<TickEvent> {
        loop {
            let tick = self.next_tick()?;
            if tick.ts_event() >= start_ts {
                return Some(tick);
            }
        }
    }
}

/// Permutation applied to ticks as they are replayed (price jitter, time scaling,
/// aggressor inference). A no-op by default; real permutations slot in here.
///
/// `apply` takes `&mut self` because realistic permutations are stateful: the
/// tick rule, for instance, must remember the prior trade's price and the last
/// inferred side per symbol to classify the current trade.
pub trait Permutation {
    fn apply(&mut self, tick: TickEvent) -> TickEvent {
        tick
    }
}

/// Identity permutation - replays the data verbatim.
#[derive(Debug, Default, Clone, Copy)]
pub struct Identity;
impl Permutation for Identity {}

/// Infers each trade's aggressor side from the tick rule: a price uptick versus
/// the prior trade of the same symbol is buyer-initiated, a downtick is
/// seller-initiated, and an unchanged price inherits the prior classification.
///
/// State is per symbol because the replay stream interleaves pairs (see
/// [`MergeSource`]); a single global "prior price" would cross-contaminate
/// symbols whose price levels are unrelated. The first trade of each symbol has
/// no predecessor, so it stays [`AggressorSide::NoAggressor`] until a direction
/// is established. Quotes pass through untouched.
#[derive(Debug, Default, Clone)]
pub struct TickRuleAggressor {
    /// Per-symbol carried state: the last trade price and the last non-neutral
    /// aggressor side inferred for it (used to resolve unchanged-price ticks).
    state: std::collections::HashMap<String, (Decimal, AggressorSide)>,
}

impl TickRuleAggressor {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Permutation for TickRuleAggressor {
    fn apply(&mut self, tick: TickEvent) -> TickEvent {
        let TickEvent::Trade(mut trade) = tick else {
            return tick;
        };
        let aggressor = match self.state.get(&trade.symbol) {
            None => AggressorSide::NoAggressor,
            Some(&(prior_price, prior_side)) => {
                use std::cmp::Ordering;
                match trade.price.cmp(&prior_price) {
                    Ordering::Greater => AggressorSide::Buyer,
                    Ordering::Less => AggressorSide::Seller,
                    Ordering::Equal => prior_side,
                }
            }
        };
        trade.aggressor = aggressor;
        // Carry forward this trade's price; carry forward a *resolved* side so an
        // unchanged-price run after a neutral first trade stays neutral until a
        // real uptick/downtick establishes direction.
        self.state
            .insert(trade.symbol.clone(), (trade.price, aggressor));
        TickEvent::Trade(trade)
    }
}

// ----------------------------------------------------------------------------
// Kraken CSV parsing (pure, IO-free - unit tested directly)
// ----------------------------------------------------------------------------

/// Parse a Kraken `time` field (unix seconds, optionally fractional) to unix ns.
///
/// Parsed as integer seconds + integer fraction rather than `f64`, because epoch
/// nanoseconds (~1.7e18) exceed `f64`'s integer precision.
///
/// Deliberately lenient at three edges that cannot occur in a real Kraken dump
/// and so are left as-is rather than tightened (this is the offline-analysis
/// lineage, and tightening only risks rejecting a valid row for no gain): a
/// trailing dot (`"1."`) parses as whole seconds with a zero fraction; a leading
/// `+` on the seconds field is accepted (Rust's integer `FromStr` allows it); and
/// [`parse_kraken_line`] ignores any columns past the third. The dump always
/// emits exactly `time,price,volume` with plain unsigned decimals, so none of
/// these shapes appears in practice.
pub fn parse_kraken_ts(field: &str) -> Option<u64> {
    let (sec, frac) = field.split_once('.').unwrap_or((field, ""));
    let secs: u64 = sec.parse().ok()?;
    let mut nanos = 0u64;
    if !frac.is_empty() {
        // Require the whole fractional field to be ASCII digits before
        // truncating to ns resolution. This rejects trailing garbage past
        // digit 9 instead of silently discarding it, and - since every ASCII
        // digit is exactly one byte - guarantees `frac.len().min(9)` always
        // lands on a char boundary, so a multi-byte character anywhere past
        // byte 9 can no longer panic the slice.
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let frac = &frac[..frac.len().min(9)]; // ns resolution
        let val: u64 = frac.parse().ok()?;
        nanos = val * 10u64.pow(9 - frac.len() as u32);
    }
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

/// Parse one Kraken CSV line into a [`TradeTick`]. Returns `None` for blank or
/// malformed lines so a stream can skip them. Aggressor side is unknown in the
/// dump, so [`AggressorSide::NoAggressor`].
pub fn parse_kraken_line(symbol: &str, line: &str) -> Option<TradeTick> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut cols = line.split(',');
    let ts_event = parse_kraken_ts(cols.next()?)?;
    let price = Decimal::from_str(cols.next()?.trim()).ok()?;
    let size = Decimal::from_str(cols.next()?.trim()).ok()?;
    // A syntactically valid but non-positive price/size is not a real trade -
    // treat it the same as a malformed line so it never reaches the replay
    // stream, where a zero/negative price would poison downstream ln-return
    // math with -inf/NaN.
    if price <= Decimal::ZERO || size <= Decimal::ZERO {
        return None;
    }
    Some(TradeTick {
        symbol: symbol.to_string(),
        price,
        size,
        aggressor: AggressorSide::NoAggressor,
        ts_event,
    })
}

/// Streaming reader over one Kraken pair CSV. Holds a single line buffer; memory
/// is constant regardless of file size.
pub struct KrakenCsvSource {
    symbol: String,
    reader: BufReader<File>,
    buf: String,
    /// `ts_event` of the last tick emitted, so a backward-stepping row (the
    /// dump is assumed sorted ascending, but nothing upstream guarantees it)
    /// can be caught here instead of silently corrupting `MergeSource`'s
    /// "each head is its source's minimum" invariant downstream.
    last_ts: Option<u64>,
}

impl KrakenCsvSource {
    /// Open a pair file, deriving the symbol from the file stem (e.g. `XBTUSD`).
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let symbol = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("UNKNOWN")
            .to_string();
        Ok(Self {
            symbol,
            reader: BufReader::new(File::open(path)?),
            buf: String::new(),
            last_ts: None,
        })
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }
}

impl TickSource for KrakenCsvSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        // Disjoint field borrows so the shared read loop can live as a free
        // function testable over an in-memory reader (see `read_next_trade`);
        // the file path itself is only exercised in production.
        read_next_trade(
            &mut self.reader,
            &mut self.buf,
            &self.symbol,
            &mut self.last_ts,
        )
        .map(TickEvent::Trade)
    }
}

/// Read forward from `reader` until the next in-order valid trade, skipping
/// blank, malformed, non-UTF-8, and backward-stepping lines. Returns `None` at
/// clean EOF or on a genuine (non-decode) read error, which truncates the
/// stream. Factored out of [`KrakenCsvSource::next_tick`] so the skip logic can
/// be unit-tested over a `Cursor` without a fixture file.
fn read_next_trade(
    reader: &mut impl BufRead,
    buf: &mut String,
    symbol: &str,
    last_ts: &mut Option<u64>,
) -> Option<TradeTick> {
    loop {
        buf.clear();
        match reader.read_line(buf) {
            Ok(0) => return None, // clean EOF ends the stream
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                // A non-UTF-8 line is a malformed ROW, not a stream-fatal IO
                // failure - so skip it and read on, exactly like every other
                // malformed-row case below, instead of truncating the whole
                // stream on one bad byte and dropping every valid trade after
                // it. `read_line` has already consumed the offending bytes (up
                // to and including the newline) even though it refused to append
                // them, so the reader is positioned at the next line. No tracing
                // dep in this crate, so stderr is the visible channel.
                eprintln!("KrakenCsvSource({symbol}): non-UTF-8 line, skipping: {err}");
                continue;
            }
            Err(err) => {
                // A genuine mid-file read error truncates the stream. The
                // streaming contract returns Option, so we still end with None -
                // but no longer silently: surface the error so a partial replay
                // is distinguishable from a complete one.
                eprintln!("KrakenCsvSource({symbol}): read error, truncating stream: {err}");
                return None;
            }
            Ok(_) => {}
        }
        if let Some(t) = parse_kraken_line(symbol, buf) {
            if let Some(last) = *last_ts
                && t.ts_event < last
            {
                // A backward-stepping row would break the ascending assumption
                // every consumer (MergeSource's k-way merge, replay pacing)
                // relies on. Skip it and keep reading rather than emit it and
                // silently let time run backwards downstream.
                eprintln!(
                    "KrakenCsvSource({symbol}): out-of-order row (ts {} < last {last}), skipping",
                    t.ts_event
                );
                continue;
            }
            *last_ts = Some(t.ts_event);
            return Some(t);
        }
        // malformed line - skip and keep reading
    }
}

/// In-memory tick source for tests and the wiring skeleton.
pub struct MemorySource {
    ticks: std::vec::IntoIter<TickEvent>,
}

impl MemorySource {
    /// Build from a tick list, sorted into replay (timestamp) order.
    pub fn new(mut ticks: Vec<TickEvent>) -> Self {
        ticks.sort_by_key(TickEvent::ts_event);
        Self {
            ticks: ticks.into_iter(),
        }
    }
}

impl TickSource for MemorySource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        self.ticks.next()
    }
}

// ----------------------------------------------------------------------------
// Multi-symbol k-way merge
// ----------------------------------------------------------------------------

/// Merges several already-time-ordered sources into one time-ordered stream.
///
/// Each input file is sorted ascending, so a heap of per-source heads yields a
/// global ordering with one buffered tick per source.
pub struct MergeSource {
    sources: Vec<Box<dyn TickSource>>,
    heads: Vec<Option<TickEvent>>,
    fault: Option<TickFault>,
}

impl MergeSource {
    pub fn new(sources: Vec<Box<dyn TickSource>>) -> Self {
        Self::starting_at(sources, None)
    }

    /// Build a merge that begins at `start_ts`, seeking each child before the
    /// first merge head is buffered.
    pub fn starting_at(sources: Vec<Box<dyn TickSource>>, start_ts: Option<u64>) -> Self {
        let heads = sources.iter().map(|_| None).collect();
        let mut s = Self {
            sources,
            heads,
            fault: None,
        };
        for i in 0..s.sources.len() {
            s.heads[i] = match start_ts {
                Some(ts) => s.sources[i].seek_to(ts),
                None => s.sources[i].next_tick(),
            };
            if s.heads[i].is_none()
                && let Some(fault) = s.sources[i].fault()
            {
                s.latch_fault(fault);
                break;
            }
        }
        s
    }

    fn latch_fault(&mut self, fault: TickFault) {
        self.fault = Some(fault);
        self.heads.fill(None);
    }
}

impl TickSource for MergeSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        if self.fault.is_some() {
            return None;
        }
        // Pick the source whose buffered head has the smallest timestamp.
        let pick = self
            .heads
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.as_ref().map(|t| (i, t.ts_event())))
            .min_by_key(|&(_, ts)| ts)
            .map(|(i, _)| i)?;
        let out = self.heads[pick].take();
        self.heads[pick] = self.sources[pick].next_tick();
        if self.heads[pick].is_none()
            && let Some(fault) = self.sources[pick].fault()
        {
            self.latch_fault(fault);
            return None;
        }
        out
    }

    fn fault(&self) -> Option<TickFault> {
        self.fault
    }

    fn arm_flow_surge(
        &mut self,
        start_ns: u64,
        duration_ms: u64,
        rate_mult: f64,
        children_mult: f64,
    ) {
        for source in &mut self.sources {
            source.arm_flow_surge(start_ns, duration_ms, rate_mult, children_mult);
        }
    }

    fn clear_flow_surge(&mut self) {
        for source in &mut self.sources {
            source.clear_flow_surge();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::Side;

    fn trade(sym: &str, ts: u64) -> TickEvent {
        TickEvent::Trade(TradeTick {
            symbol: sym.into(),
            price: Decimal::from(100),
            size: Decimal::from(1),
            aggressor: AggressorSide::NoAggressor,
            ts_event: ts,
        })
    }

    fn trade_at(sym: &str, ts: u64, price: i64) -> TickEvent {
        TickEvent::Trade(TradeTick {
            symbol: sym.into(),
            price: Decimal::from(price),
            size: Decimal::from(1),
            aggressor: AggressorSide::NoAggressor,
            ts_event: ts,
        })
    }

    fn aggressor_of(tick: &TickEvent) -> AggressorSide {
        match tick {
            TickEvent::Trade(t) => t.aggressor,
            TickEvent::Quote(_) => unreachable!("test only feeds trades"),
        }
    }

    #[test]
    fn memory_source_replays_in_time_order() {
        let mut src = MemorySource::new(vec![trade("A", 30), trade("A", 10), trade("A", 20)]);
        let order: Vec<u64> =
            std::iter::from_fn(|| src.next_tick().map(|t| t.ts_event())).collect();
        assert_eq!(order, vec![10, 20, 30]);
    }

    struct FaultAfterFirst {
        first: Option<TickEvent>,
        faulted: bool,
    }

    impl TickSource for FaultAfterFirst {
        fn next_tick(&mut self) -> Option<TickEvent> {
            if let Some(tick) = self.first.take() {
                return Some(tick);
            }
            self.faulted = true;
            None
        }

        fn fault(&self) -> Option<TickFault> {
            self.faulted
                .then_some(TickFault::Arrival(ArrivalRefusal::NoOpenExposure {
                    from_ns: 42,
                }))
        }
    }

    #[test]
    fn merge_source_latches_a_child_fault_and_discards_other_heads() {
        let faulting = FaultAfterFirst {
            first: Some(trade("fault", 10)),
            faulted: false,
        };
        let healthy = MemorySource::new(vec![trade("healthy", 20), trade("healthy", 30)]);
        let mut merged = MergeSource::new(vec![Box::new(faulting), Box::new(healthy)]);

        assert!(merged.next_tick().is_none());
        assert_eq!(
            merged.fault(),
            Some(TickFault::Arrival(ArrivalRefusal::NoOpenExposure {
                from_ns: 42,
            }))
        );
        assert!(merged.next_tick().is_none());
    }

    #[test]
    fn parses_integer_and_fractional_timestamps() {
        assert_eq!(
            parse_kraken_ts("1743439968"),
            Some(1_743_439_968_000_000_000)
        );
        assert_eq!(
            parse_kraken_ts("1660044887.5"),
            Some(1_660_044_887_500_000_000)
        );
        assert_eq!(parse_kraken_ts("1.123456789"), Some(1_123_456_789));
        assert_eq!(parse_kraken_ts("notanumber"), None);
    }

    #[test]
    fn fractional_timestamp_with_multibyte_char_does_not_panic() {
        // Regression: `frac[..frac.len().min(9)]` used to slice by byte index
        // regardless of char boundaries. "12345678é9" is 11 bytes with `é`
        // straddling bytes 8..10, so a naive byte-9 slice lands mid-character.
        // The field is correctly rejected (non-digit), but must not panic.
        assert_eq!(parse_kraken_ts("1.12345678é9"), None);
    }

    #[test]
    fn fractional_timestamp_rejects_trailing_garbage() {
        // Previously the field was truncated to 9 digits before validation, so
        // garbage past digit 9 was silently discarded instead of failing the
        // field like garbage within the first 9 digits already did.
        assert_eq!(parse_kraken_ts("1.123456789xyzzy"), None);
    }

    #[test]
    fn parses_kraken_trade_line() {
        let t = parse_kraken_line("XBTUSD", "1743439968,4.8000000,2.63045\n").unwrap();
        assert_eq!(t.symbol, "XBTUSD");
        assert_eq!(t.price, Decimal::from_str("4.8000000").unwrap());
        assert_eq!(t.size, Decimal::from_str("2.63045").unwrap());
        assert_eq!(t.aggressor, AggressorSide::NoAggressor);
        assert_eq!(t.ts_event, 1_743_439_968_000_000_000);
        // order side is unaffected by trade aggressor representation
        assert_ne!(Side::Buy, Side::Sell);
    }

    #[test]
    fn blank_and_malformed_lines_are_skipped() {
        assert!(parse_kraken_line("X", "").is_none());
        assert!(parse_kraken_line("X", "   ").is_none());
        assert!(parse_kraken_line("X", "1743439968,onlytwo").is_none());
    }

    #[test]
    fn nonpositive_price_or_size_is_rejected() {
        assert!(parse_kraken_line("X", "1743439968,-4.8,2.6").is_none());
        assert!(parse_kraken_line("X", "1743439968,4.8,0").is_none());
        assert!(parse_kraken_line("X", "1743439968,0,2.6").is_none());
    }

    #[test]
    fn non_utf8_line_is_skipped_not_fatal() {
        // Regression: a non-UTF-8 line made `read_line` return
        // `ErrorKind::InvalidData`, which used to be handled as a fatal read
        // error truncating the whole stream - so one bad byte dropped every
        // valid trade after it, unlike every other malformed-row case which is
        // skipped. Now it skips-with-warn, so a valid trade AFTER the bad line
        // still comes through.
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(b"1743439968,4.8,2.6\n");
        data.extend_from_slice(&[0xff, 0xfe, b'g', b'a', b'r', b'b', b'\n']); // invalid UTF-8
        data.extend_from_slice(b"1743439969,4.9,2.7\n");
        let mut reader = std::io::Cursor::new(data);
        let mut buf = String::new();
        let mut last_ts = None;

        let first =
            read_next_trade(&mut reader, &mut buf, "X", &mut last_ts).expect("first trade parses");
        assert_eq!(first.ts_event, 1_743_439_968_000_000_000);
        // The non-UTF-8 line between the two trades is skipped, not fatal.
        let second =
            read_next_trade(&mut reader, &mut buf, "X", &mut last_ts).expect("second trade parses");
        assert_eq!(second.ts_event, 1_743_439_969_000_000_000);
        assert!(read_next_trade(&mut reader, &mut buf, "X", &mut last_ts).is_none());
    }

    #[test]
    fn merge_interleaves_symbols_by_timestamp() {
        let a = Box::new(MemorySource::new(vec![trade("A", 10), trade("A", 40)]));
        let b = Box::new(MemorySource::new(vec![
            trade("B", 20),
            trade("B", 30),
            trade("B", 50),
        ]));
        let mut m = MergeSource::new(vec![a, b]);
        let mut seen = Vec::new();
        while let Some(t) = m.next_tick() {
            let TickEvent::Trade(tt) = t else {
                unreachable!()
            };
            seen.push((tt.symbol, tt.ts_event));
        }
        assert_eq!(
            seen,
            vec![
                ("A".into(), 10),
                ("B".into(), 20),
                ("B".into(), 30),
                ("A".into(), 40),
                ("B".into(), 50),
            ]
        );
    }

    #[test]
    fn seek_to_skips_prefix_and_returns_first_in_window() {
        let mut src = MemorySource::new(vec![
            trade("A", 10),
            trade("A", 20),
            trade("A", 30),
            trade("A", 40),
        ]);

        assert_eq!(src.seek_to(25).map(|t| t.ts_event()), Some(30));
        assert_eq!(src.next_tick().map(|t| t.ts_event()), Some(40));
        assert!(src.next_tick().is_none());
    }

    #[test]
    fn seek_to_past_end_returns_none() {
        let mut src = MemorySource::new(vec![
            trade("A", 10),
            trade("A", 20),
            trade("A", 30),
            trade("A", 40),
        ]);

        assert!(src.seek_to(999).is_none());
    }

    #[test]
    fn identity_permutation_leaves_aggressor_untouched() {
        let mut perm = Identity;
        let out = perm.apply(trade_at("XBTUSD", 10, 100));
        assert_eq!(aggressor_of(&out), AggressorSide::NoAggressor);
    }

    #[test]
    fn tick_rule_first_trade_is_neutral() {
        let mut perm = TickRuleAggressor::new();
        let out = perm.apply(trade_at("XBTUSD", 10, 100));
        assert_eq!(aggressor_of(&out), AggressorSide::NoAggressor);
    }

    #[test]
    fn tick_rule_classifies_up_and_down_ticks() {
        let mut perm = TickRuleAggressor::new();
        perm.apply(trade_at("XBTUSD", 10, 100)); // first, neutral
        assert_eq!(
            aggressor_of(&perm.apply(trade_at("XBTUSD", 20, 101))),
            AggressorSide::Buyer
        );
        assert_eq!(
            aggressor_of(&perm.apply(trade_at("XBTUSD", 30, 100))),
            AggressorSide::Seller
        );
    }

    #[test]
    fn tick_rule_unchanged_price_inherits_prior_side() {
        let mut perm = TickRuleAggressor::new();
        perm.apply(trade_at("XBTUSD", 10, 100)); // neutral
        perm.apply(trade_at("XBTUSD", 20, 101)); // buyer (uptick)
        // unchanged price inherits the prior (buyer) classification
        assert_eq!(
            aggressor_of(&perm.apply(trade_at("XBTUSD", 30, 101))),
            AggressorSide::Buyer
        );
        perm.apply(trade_at("XBTUSD", 40, 100)); // seller (downtick)
        assert_eq!(
            aggressor_of(&perm.apply(trade_at("XBTUSD", 50, 100))),
            AggressorSide::Seller
        );
    }

    #[test]
    fn tick_rule_unchanged_price_after_neutral_first_stays_neutral() {
        let mut perm = TickRuleAggressor::new();
        perm.apply(trade_at("XBTUSD", 10, 100)); // neutral, no predecessor
        // second trade at the same price has no direction to inherit yet
        assert_eq!(
            aggressor_of(&perm.apply(trade_at("XBTUSD", 20, 100))),
            AggressorSide::NoAggressor
        );
    }

    #[test]
    fn tick_rule_tracks_symbols_independently() {
        let mut perm = TickRuleAggressor::new();
        perm.apply(trade_at("XBTUSD", 10, 100));
        perm.apply(trade_at("ETHUSD", 15, 50));
        // XBT upticks while ETH downticks - neither price level bleeds into the other
        assert_eq!(
            aggressor_of(&perm.apply(trade_at("XBTUSD", 20, 101))),
            AggressorSide::Buyer
        );
        assert_eq!(
            aggressor_of(&perm.apply(trade_at("ETHUSD", 25, 49))),
            AggressorSide::Seller
        );
    }

    #[test]
    fn merge_starting_at_windows_each_source() {
        let a = Box::new(MemorySource::new(vec![trade("A", 10), trade("A", 40)]));
        let b = Box::new(MemorySource::new(vec![
            trade("B", 20),
            trade("B", 30),
            trade("B", 50),
        ]));
        let mut m = MergeSource::starting_at(vec![a, b], Some(25));
        let order: Vec<u64> = std::iter::from_fn(|| m.next_tick().map(|t| t.ts_event())).collect();

        assert_eq!(order, vec![30, 40, 50]);
    }
}
