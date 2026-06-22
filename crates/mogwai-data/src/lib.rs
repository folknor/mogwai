//! Tick storage + replay. The replay engine pulls ticks in time order and the
//! server fans them out to subscribers as market data.
//!
//! Backed by the Kraken trade-history CSV dump: one file per pair (symbol taken
//! from the file stem), no header, three columns `time,price,volume` where time
//! is unix **seconds** (optionally fractional). Files reach multiple GB, so
//! [`KrakenCsvSource`] streams one buffered line at a time - O(1) memory - and
//! [`MergeSource`] k-way merges several symbols into one time-ordered stream.

use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    path::Path,
    str::FromStr,
};

use mogwai_protocol::{AggressorSide, QuoteTick, TradeTick};
use rust_decimal::Decimal;

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

    /// Advance past ticks before `start_ts`, returning the first tick in the
    /// window. The default drains one tick at a time and keeps O(1) memory.
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
pub trait Permutation {
    fn apply(&self, tick: TickEvent) -> TickEvent {
        tick
    }
}

/// Identity permutation - replays the data verbatim.
#[derive(Debug, Default, Clone, Copy)]
pub struct Identity;
impl Permutation for Identity {}

// ----------------------------------------------------------------------------
// Kraken CSV parsing (pure, IO-free - unit tested directly)
// ----------------------------------------------------------------------------

/// Parse a Kraken `time` field (unix seconds, optionally fractional) to unix ns.
///
/// Parsed as integer seconds + integer fraction rather than `f64`, because epoch
/// nanoseconds (~1.7e18) exceed `f64`'s integer precision.
pub fn parse_kraken_ts(field: &str) -> Option<u64> {
    let (sec, frac) = field.split_once('.').unwrap_or((field, ""));
    let secs: u64 = sec.parse().ok()?;
    let mut nanos = 0u64;
    if !frac.is_empty() {
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
        })
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }
}

impl TickSource for KrakenCsvSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        loop {
            self.buf.clear();
            match self.reader.read_line(&mut self.buf) {
                Ok(0) | Err(_) => return None, // EOF or IO error ends the stream
                Ok(_) => {}
            }
            if let Some(t) = parse_kraken_line(&self.symbol, &self.buf) {
                return Some(TickEvent::Trade(t));
            }
            // malformed line - skip and keep reading
        }
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
}

impl MergeSource {
    pub fn new(sources: Vec<Box<dyn TickSource>>) -> Self {
        Self::starting_at(sources, None)
    }

    /// Build a merge that begins at `start_ts`, seeking each child before the
    /// first merge head is buffered.
    pub fn starting_at(sources: Vec<Box<dyn TickSource>>, start_ts: Option<u64>) -> Self {
        let heads = sources.iter().map(|_| None).collect();
        let mut s = Self { sources, heads };
        for i in 0..s.sources.len() {
            s.heads[i] = match start_ts {
                Some(ts) => s.sources[i].seek_to(ts),
                None => s.sources[i].next_tick(),
            };
        }
        s
    }
}

impl TickSource for MergeSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
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
        out
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

    #[test]
    fn memory_source_replays_in_time_order() {
        let mut src = MemorySource::new(vec![trade("A", 30), trade("A", 10), trade("A", 20)]);
        let order: Vec<u64> =
            std::iter::from_fn(|| src.next_tick().map(|t| t.ts_event())).collect();
        assert_eq!(order, vec![10, 20, 30]);
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
