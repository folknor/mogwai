// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The compact record representation of one river's tape, and the two
//! interfaces that read it.
//!
//! A [`Record`] carries no symbol and no `Decimal`: its prices and quantities
//! are integer mantissas at a decimal scale fixed for the whole river, held
//! once in a [`RecordScale`]. A protocol object - a `TradeTick`, a
//! `QuoteTick`, a JSON frame - is built from a record and its scale only at the
//! edge where something actually needs one, which is what makes the generator
//! cheap to read: before this representation every record went f64 to
//! `Decimal` to `String` to `Arc<str>`, each stage discarding the structure the
//! previous one had.
//!
//! The scale belongs to a river, so the compact interface is single-river by
//! construction. [`RiverSource`] is it: one river, one scale for its life.
//! Heterogeneous streams - the offline Kraken lineage, a multi-symbol merge, a
//! composed segment river - stay on the event-facing [`crate::TickSource`],
//! where each event carries its own symbol and exact decimals, and
//! [`EventView`] adapts a river into that world rather than the reverse.
//!
//! [`ParentSource`] is the parent-level view a measurement reads when it needs
//! to know where one parent transaction ends. Quotes are not a parent boundary
//! signal - the discrete-book path can publish two quotes for one parent or
//! suppress both - so a consumer that needs parents asks for them rather than
//! inferring them from the record stream.

use std::fmt::Write as _;

use mogwai_protocol::{AggressorSide, QuoteTick, Symbol, TradeTick};
use rust_decimal::Decimal;

use crate::{TickEvent, TickFault, TickSource, VolTrace};

/// One print. `px` is a price mantissa at the river's `price_dp`, `qty` a
/// quantity mantissa at its `size_dp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeRec {
    pub ts: u64,
    pub px: i64,
    pub qty: u64,
    pub side: AggressorSide,
}

/// One top-of-book publication, at the same scales as [`TradeRec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuoteRec {
    pub ts: u64,
    pub bid_px: i64,
    pub ask_px: i64,
    pub bid_qty: u64,
    pub ask_qty: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record {
    Trade(TradeRec),
    Quote(QuoteRec),
}

impl Record {
    #[must_use]
    pub fn ts(&self) -> u64 {
        match self {
            Record::Trade(trade) => trade.ts,
            Record::Quote(quote) => quote.ts,
        }
    }
}

/// The largest decimal scale a record mantissa is read at. `Decimal` carries
/// at most 28 fractional digits, but the binding limit is the integer: an
/// `i64` price mantissa at more than 18 places cannot even hold one unit of
/// the integer part, so a scale past it describes no usable river.
pub const MAX_RECORD_DP: u32 = 18;

/// Why a [`RecordScale`] or a record built against one was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleError {
    /// A decimal scale above [`MAX_RECORD_DP`].
    Scale { field: &'static str, dp: u32 },
    /// A configured or supplied decimal that is not exact at the river's
    /// scale, or whose mantissa does not fit the record's integer.
    Unrepresentable { field: &'static str, value: Decimal },
}

impl std::fmt::Display for ScaleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scale { field, dp } => write!(
                f,
                "{field} scale {dp} exceeds the record limit of {MAX_RECORD_DP} places"
            ),
            Self::Unrepresentable { field, value } => write!(
                f,
                "{field} {value} is not exactly representable at the river's record scale"
            ),
        }
    }
}

impl std::error::Error for ScaleError {}

/// The fixed decimal scales and the symbol of one river.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordScale {
    symbol: Symbol,
    /// The symbol already rendered as a JSON string literal by the real
    /// escaper, so the frame writer never interpolates raw text: the symbol
    /// set is open, and a quote or a backslash in a label must reach the wire
    /// escaped exactly as `serde_json` would have escaped it.
    symbol_json: Box<str>,
    price_dp: u32,
    size_dp: u32,
    price_div: f64,
}

impl RecordScale {
    pub fn new(symbol: Symbol, price_dp: u32, size_dp: u32) -> Result<Self, ScaleError> {
        if price_dp > MAX_RECORD_DP {
            return Err(ScaleError::Scale {
                field: "price",
                dp: price_dp,
            });
        }
        if size_dp > MAX_RECORD_DP {
            return Err(ScaleError::Scale {
                field: "size",
                dp: size_dp,
            });
        }
        // A `str` always serializes; the fallback is unreachable and still
        // escapes nothing it should not, because it is empty.
        let symbol_json = serde_json::to_string(&*symbol)
            .unwrap_or_default()
            .into_boxed_str();
        Ok(Self {
            symbol,
            symbol_json,
            price_dp,
            size_dp,
            price_div: 10_f64.powi(price_dp as i32),
        })
    }

    #[must_use]
    pub fn symbol(&self) -> &Symbol {
        &self.symbol
    }

    #[must_use]
    pub fn price_dp(&self) -> u32 {
        self.price_dp
    }

    #[must_use]
    pub fn size_dp(&self) -> u32 {
        self.size_dp
    }

    /// A price mantissa as the exact decimal it denotes, at the river's scale.
    #[must_use]
    pub fn price(&self, mantissa: i64) -> Decimal {
        Decimal::from_i128_with_scale(i128::from(mantissa), self.price_dp)
    }

    /// A quantity mantissa as the exact decimal it denotes.
    #[must_use]
    pub fn size(&self, mantissa: u64) -> Decimal {
        Decimal::from_i128_with_scale(i128::from(mantissa), self.size_dp)
    }

    /// A price mantissa as a float, by one division by `10^price_dp`. The one
    /// conversion every float reader of a river's prices uses, so two readers
    /// of the same record can never disagree in the last bit.
    #[must_use]
    pub fn price_f64(&self, mantissa: i64) -> f64 {
        mantissa as f64 / self.price_div
    }

    /// The mantissa of an exact decimal at the price scale, or `None` when the
    /// value carries more places than the scale or overflows the record.
    #[must_use]
    pub fn price_mantissa(&self, value: Decimal) -> Option<i64> {
        exact_mantissa(value, self.price_dp).and_then(|m| i64::try_from(m).ok())
    }

    /// The mantissa of an exact non-negative decimal at the size scale.
    #[must_use]
    pub fn size_mantissa(&self, value: Decimal) -> Option<u64> {
        exact_mantissa(value, self.size_dp).and_then(|m| u64::try_from(m).ok())
    }

    /// An arbitrary decimal threshold compiled onto the price scale, so a
    /// predicate over record mantissas decides exactly what the same
    /// predicate over the decimals would.
    ///
    /// The direction is the predicate's, not a rounding preference. With
    /// integer prices, `price < 100.2` admits 100; rounding the threshold to
    /// 100 and keeping `<` would lose that hit. So a strict-less or
    /// inclusive-greater predicate takes the ceiling, and an inclusive-less or
    /// strict-greater one takes the floor:
    ///
    /// - `price < t` iff `m < ceil(t)`
    /// - `price <= t` iff `m <= floor(t)`
    /// - `price > t` iff `m > floor(t)`
    /// - `price >= t` iff `m >= ceil(t)`
    ///
    /// Computed in `i128`, saturating, so a threshold outside the record's
    /// range still compares correctly against every `i64` mantissa.
    #[must_use]
    pub fn price_threshold(&self, value: Decimal, round: Round) -> i128 {
        let mantissa = value.mantissa();
        let scale = value.scale();
        if scale <= self.price_dp {
            let up = 10_i128.checked_pow(self.price_dp - scale);
            return up
                .and_then(|up| mantissa.checked_mul(up))
                .unwrap_or(if mantissa < 0 { i128::MIN } else { i128::MAX });
        }
        let down = 10_i128.pow(scale - self.price_dp);
        match round {
            Round::Floor => mantissa.div_euclid(down),
            Round::Ceil => -((-mantissa).div_euclid(down)),
        }
    }

    #[must_use]
    pub fn trade(&self, trade: &TradeRec) -> TradeTick {
        TradeTick {
            symbol: std::sync::Arc::clone(&self.symbol),
            price: self.price(trade.px),
            size: self.size(trade.qty),
            aggressor: trade.side,
            ts_event: trade.ts,
        }
    }

    #[must_use]
    pub fn quote(&self, quote: &QuoteRec) -> QuoteTick {
        QuoteTick {
            symbol: std::sync::Arc::clone(&self.symbol),
            bid_px: self.price(quote.bid_px),
            ask_px: self.price(quote.ask_px),
            bid_sz: self.size(quote.bid_qty),
            ask_sz: self.size(quote.ask_qty),
            ts_event: quote.ts,
        }
    }

    /// The edge view of one record: the protocol event a heterogeneous
    /// consumer reads.
    #[must_use]
    pub fn event(&self, record: &Record) -> TickEvent {
        match record {
            Record::Trade(trade) => TickEvent::Trade(self.trade(trade)),
            Record::Quote(quote) => TickEvent::Quote(self.quote(quote)),
        }
    }

    /// Append the record's `VenueMessage` JSON frame to `out`, byte for byte
    /// what `serde_json::to_string` of the edge view produces, without
    /// building the view. `the_frame_writer_matches_serde` pins the
    /// equivalence over boundary values; the decimal fields are written at the
    /// river's fixed scale, which is what `Decimal`'s own `Display` does for a
    /// value built by [`RecordScale::price`] and [`RecordScale::size`].
    pub fn write_frame(&self, record: &Record, out: &mut String) {
        match record {
            Record::Trade(trade) => {
                out.push_str("{\"type\":\"Trade\",\"symbol\":");
                out.push_str(&self.symbol_json);
                out.push_str(",\"price\":\"");
                push_fixed(out, i128::from(trade.px), self.price_dp);
                out.push_str("\",\"size\":\"");
                push_fixed(out, i128::from(trade.qty), self.size_dp);
                out.push_str("\",\"aggressor\":\"");
                out.push_str(match trade.side {
                    AggressorSide::NoAggressor => "NoAggressor",
                    AggressorSide::Buyer => "Buyer",
                    AggressorSide::Seller => "Seller",
                });
                out.push_str("\",\"ts_event\":");
                write!(out, "{}", trade.ts).expect("writing to a String cannot fail");
                out.push('}');
            }
            Record::Quote(quote) => {
                out.push_str("{\"type\":\"Quote\",\"symbol\":");
                out.push_str(&self.symbol_json);
                out.push_str(",\"bid_px\":\"");
                push_fixed(out, i128::from(quote.bid_px), self.price_dp);
                out.push_str("\",\"ask_px\":\"");
                push_fixed(out, i128::from(quote.ask_px), self.price_dp);
                out.push_str("\",\"bid_sz\":\"");
                push_fixed(out, i128::from(quote.bid_qty), self.size_dp);
                out.push_str("\",\"ask_sz\":\"");
                push_fixed(out, i128::from(quote.ask_qty), self.size_dp);
                out.push_str("\",\"ts_event\":");
                write!(out, "{}", quote.ts).expect("writing to a String cannot fail");
                out.push('}');
            }
        }
    }
}

/// The direction a decimal threshold is compiled in; see
/// [`RecordScale::price_threshold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Round {
    Floor,
    Ceil,
}

/// `value * 10^dp` when it is an integer, in `i128`.
fn exact_mantissa(value: Decimal, dp: u32) -> Option<i128> {
    let mantissa = value.mantissa();
    let scale = value.scale();
    if scale <= dp {
        mantissa.checked_mul(10_i128.checked_pow(dp - scale)?)
    } else {
        let down = 10_i128.pow(scale - dp);
        (mantissa % down == 0).then(|| mantissa / down)
    }
}

/// A mantissa written as a fixed-point decimal with exactly `dp` places.
fn push_fixed(out: &mut String, mantissa: i128, dp: u32) {
    if mantissa < 0 {
        out.push('-');
    }
    let magnitude = mantissa.unsigned_abs();
    if dp == 0 {
        write!(out, "{magnitude}").expect("writing to a String cannot fail");
        return;
    }
    let unit = 10_u128.pow(dp);
    write!(
        out,
        "{}.{:0width$}",
        magnitude / unit,
        magnitude % unit,
        width = dp as usize
    )
    .expect("writing to a String cannot fail");
}

/// One river's tape as compact records, in replay order.
///
/// The scale is fixed for the source's life: every record it returns is read
/// against the one [`RecordScale`] it reports.
pub trait RiverSource {
    fn scale(&self) -> &RecordScale;

    /// The next record, or `None` at the end of the river or on a fault.
    fn next_record(&mut self) -> Option<Record>;

    /// The terminal fault that ended this river, if any. `None` after a
    /// terminal `None` from `next_record` means ordinary exhaustion; a reader
    /// that treats exhaustion as proof that a span is complete must ask this
    /// first, because a fault proves nothing about the span it cut off.
    fn fault(&self) -> Option<TickFault> {
        None
    }

    /// Consume records before `start_ts` and return the first at or after it.
    /// The default drains one record at a time; a source with a cheaper skip
    /// overrides it.
    fn seek_to(&mut self, start_ts: u64) -> Option<Record> {
        loop {
            let record = self.next_record()?;
            if record.ts() >= start_ts {
                return Some(record);
            }
        }
    }
}

impl<R: RiverSource + ?Sized> RiverSource for Box<R> {
    fn scale(&self) -> &RecordScale {
        (**self).scale()
    }
    fn next_record(&mut self) -> Option<Record> {
        (**self).next_record()
    }
    fn fault(&self) -> Option<TickFault> {
        (**self).fault()
    }
    fn seek_to(&mut self, start_ts: u64) -> Option<Record> {
        (**self).seek_to(start_ts)
    }
}

impl<R: RiverSource + ?Sized> RiverSource for &mut R {
    fn scale(&self) -> &RecordScale {
        (**self).scale()
    }
    fn next_record(&mut self) -> Option<Record> {
        (**self).next_record()
    }
    fn fault(&self) -> Option<TickFault> {
        (**self).fault()
    }
    fn seek_to(&mut self, start_ts: u64) -> Option<Record> {
        (**self).seek_to(start_ts)
    }
}

/// One whole parent transaction, as a parent-aware reader sees it.
#[derive(Debug, Clone, Copy)]
pub struct ParentView<'a> {
    /// The parent's own instant: its first reserved slot.
    pub parent_ts: u64,
    /// Every record the parent emits, in order. Never empty.
    pub records: &'a [Record],
    /// How many of `records` are trades.
    pub trades: u32,
    /// The parent's volatility intermediates, when the source traces them.
    pub trace: Option<VolTrace>,
}

/// A river that can be read a whole parent at a time.
///
/// `next_parent` is only defined at a parent boundary and consumes the entire
/// parent it returns. Mixing it with `next_record` is allowed only at
/// boundaries: a reader that has taken part of a parent record by record and
/// then asks for a parent would otherwise receive either a duplicate or a
/// fragment, and both are wrong. Implementations refuse that call by panicking
/// with the misuse named.
pub trait ParentSource: RiverSource {
    fn next_parent(&mut self) -> Option<ParentView<'_>>;
}

/// A river as a heterogeneous event stream: the edge adapter that lets a
/// record source feed code written against [`TickSource`].
pub struct EventView<R> {
    river: R,
}

impl<R: RiverSource> EventView<R> {
    pub fn new(river: R) -> Self {
        Self { river }
    }

    pub fn into_inner(self) -> R {
        self.river
    }

    pub fn river(&self) -> &R {
        &self.river
    }
}

impl<R: RiverSource> TickSource for EventView<R> {
    fn next_tick(&mut self) -> Option<TickEvent> {
        let record = self.river.next_record()?;
        Some(self.river.scale().event(&record))
    }

    fn fault(&self) -> Option<TickFault> {
        self.river.fault()
    }

    fn seek_to(&mut self, start_ts: u64) -> Option<TickEvent> {
        let record = self.river.seek_to(start_ts)?;
        Some(self.river.scale().event(&record))
    }
}

/// A river positioned past a record its positioning consumed, which it hands
/// back first.
///
/// `seek_to` consumes the record it returns, so whatever positions a river at
/// an inclusive start has to hold that record until the stream is read.
/// Dropping it would silently start the window one record late - exactly at
/// the requested instant, the record a caller asking for `from + 1` relies on
/// receiving.
pub struct Pushback<R> {
    first: Option<Record>,
    river: R,
}

impl<R: RiverSource> Pushback<R> {
    /// Position `river` at `start` (inclusive) and hold the record found
    /// there. `None` positions nothing and reads the river from where it is.
    pub fn starting_at(mut river: R, start: Option<u64>) -> Self {
        let first = start.and_then(|start| river.seek_to(start));
        Self { first, river }
    }
}

impl<R: RiverSource> RiverSource for Pushback<R> {
    fn scale(&self) -> &RecordScale {
        self.river.scale()
    }

    fn next_record(&mut self) -> Option<Record> {
        self.first.take().or_else(|| self.river.next_record())
    }

    fn fault(&self) -> Option<TickFault> {
        self.river.fault()
    }
}

/// An in-memory river for tests: records in replay order at one declared
/// scale.
#[derive(Debug, Clone)]
pub struct MemoryRiver {
    scale: RecordScale,
    records: std::vec::IntoIter<Record>,
}

impl MemoryRiver {
    /// Records sorted into replay order, stably, so ties keep their given
    /// order - a quote and the trade sharing its instant stay as written.
    #[must_use]
    pub fn new(scale: RecordScale, mut records: Vec<Record>) -> Self {
        records.sort_by_key(Record::ts);
        Self {
            scale,
            records: records.into_iter(),
        }
    }

    /// Build from protocol events, converting every decimal exactly onto the
    /// declared scale and refusing any that is not representable there or
    /// that names another symbol.
    pub fn from_events(scale: RecordScale, events: &[TickEvent]) -> Result<Self, ScaleError> {
        let records = events
            .iter()
            .map(|event| record_from_event(&scale, event))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(scale, records))
    }
}

/// One protocol event converted exactly onto a river's scale.
pub fn record_from_event(scale: &RecordScale, event: &TickEvent) -> Result<Record, ScaleError> {
    let price = |field, value| {
        scale
            .price_mantissa(value)
            .ok_or(ScaleError::Unrepresentable { field, value })
    };
    let size = |field, value| {
        scale
            .size_mantissa(value)
            .ok_or(ScaleError::Unrepresentable { field, value })
    };
    Ok(match event {
        TickEvent::Trade(trade) => Record::Trade(TradeRec {
            ts: trade.ts_event,
            px: price("price", trade.price)?,
            qty: size("size", trade.size)?,
            side: trade.aggressor,
        }),
        TickEvent::Quote(quote) => Record::Quote(QuoteRec {
            ts: quote.ts_event,
            bid_px: price("bid_px", quote.bid_px)?,
            ask_px: price("ask_px", quote.ask_px)?,
            bid_qty: size("bid_sz", quote.bid_sz)?,
            ask_qty: size("ask_sz", quote.ask_sz)?,
        }),
    })
}

impl RiverSource for MemoryRiver {
    fn scale(&self) -> &RecordScale {
        &self.scale
    }

    fn next_record(&mut self) -> Option<Record> {
        self.records.next()
    }
}

/// An in-memory river with explicit parent divisions, for parent-aware
/// readers' tests. Each inner vector is one parent and must be non-empty.
#[derive(Debug, Clone)]
pub struct MemoryParents {
    scale: RecordScale,
    parents: std::collections::VecDeque<Vec<Record>>,
    current: Vec<Record>,
    cursor: usize,
}

impl MemoryParents {
    #[must_use]
    pub fn new(scale: RecordScale, parents: Vec<Vec<Record>>) -> Self {
        assert!(
            parents.iter().all(|parent| !parent.is_empty()),
            "a parent emits at least one record"
        );
        Self {
            scale,
            parents: parents.into(),
            current: Vec::new(),
            cursor: 0,
        }
    }
}

impl RiverSource for MemoryParents {
    fn scale(&self) -> &RecordScale {
        &self.scale
    }

    fn next_record(&mut self) -> Option<Record> {
        if self.cursor == self.current.len() {
            self.current = self.parents.pop_front()?;
            self.cursor = 0;
        }
        let record = self.current[self.cursor];
        self.cursor += 1;
        Some(record)
    }
}

impl ParentSource for MemoryParents {
    fn next_parent(&mut self) -> Option<ParentView<'_>> {
        assert!(
            self.cursor == self.current.len(),
            "next_parent called mid-parent, after a partial record read"
        );
        self.current = self.parents.pop_front()?;
        self.cursor = self.current.len();
        Some(ParentView {
            parent_ts: self.current[0].ts(),
            records: &self.current,
            trades: self
                .current
                .iter()
                .filter(|record| matches!(record, Record::Trade(_)))
                .count() as u32,
            trace: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::VenueMessage;

    fn scale(symbol: &str, price_dp: u32, size_dp: u32) -> RecordScale {
        RecordScale::new(Symbol::from(symbol), price_dp, size_dp).unwrap()
    }

    fn serde_frame(scale: &RecordScale, record: &Record) -> String {
        let message = match scale.event(record) {
            TickEvent::Trade(trade) => VenueMessage::Trade(trade),
            TickEvent::Quote(quote) => VenueMessage::Quote(quote),
        };
        serde_json::to_string(&message).unwrap()
    }

    /// The direct writer is the serde path, byte for byte, over the edges
    /// that differ in spelling: zero, one unit, the integer extremes, scale
    /// zero and the largest scale, every aggressor, and symbols that need
    /// escaping. The raw decimal strings are asserted as well, because
    /// decoding alone accepts `0.0027` and `0.00270000` alike.
    #[test]
    fn the_frame_writer_matches_serde() {
        let symbols = [
            "BTCUSDT",
            "A\"B",
            "back\\slash",
            "uni\u{e9}",
            "tab\there",
            "",
        ];
        let mantissas = [0_i64, 1, 9, 10, 27, 1_000_000_007, i64::MAX, i64::MIN + 1];
        let sizes = [0_u64, 1, 270_000, u64::MAX / 2, 1_000_000_000_000_000_000];
        for symbol in symbols {
            for (price_dp, size_dp) in [(0, 0), (2, 8), (9, 18), (18, 0)] {
                let scale = scale(symbol, price_dp, size_dp);
                for px in mantissas {
                    for qty in sizes {
                        for side in [
                            AggressorSide::Buyer,
                            AggressorSide::Seller,
                            AggressorSide::NoAggressor,
                        ] {
                            let records = [
                                Record::Trade(TradeRec {
                                    ts: px.unsigned_abs(),
                                    px,
                                    qty,
                                    side,
                                }),
                                Record::Quote(QuoteRec {
                                    ts: u64::MAX,
                                    bid_px: px,
                                    ask_px: px.saturating_add(1),
                                    bid_qty: qty,
                                    ask_qty: qty / 3,
                                }),
                            ];
                            for record in records {
                                let mut written = String::new();
                                scale.write_frame(&record, &mut written);
                                assert_eq!(written, serde_frame(&scale, &record));
                                let decoded: VenueMessage = serde_json::from_str(&written).unwrap();
                                assert!(matches!(
                                    decoded,
                                    VenueMessage::Trade(_) | VenueMessage::Quote(_)
                                ));
                            }
                        }
                    }
                }
            }
        }
        let btc = scale("BTCUSDT", 2, 8);
        let mut written = String::new();
        btc.write_frame(
            &Record::Trade(TradeRec {
                ts: 5,
                px: 9_500_010,
                qty: 270_000,
                side: AggressorSide::Buyer,
            }),
            &mut written,
        );
        assert_eq!(
            written,
            r#"{"type":"Trade","symbol":"BTCUSDT","price":"95000.10","size":"0.00270000","aggressor":"Buyer","ts_event":5}"#
        );
    }

    /// Each compiled threshold decides exactly what the decimal predicate
    /// decides, for on-grid and off-grid thresholds, negative ones, and ones
    /// far outside the record range.
    #[test]
    fn compiled_thresholds_decide_what_the_decimal_predicates_decide() {
        let scale = scale("X", 2, 0);
        let thresholds = [
            "100.2", "100.20", "100", "-0.005", "99.999", "100.001", "0", "1e20", "-1e20",
        ];
        for text in thresholds {
            let t: Decimal = if text.contains('e') {
                Decimal::from_scientific(text).unwrap()
            } else {
                text.parse().unwrap()
            };
            let ceil = scale.price_threshold(t, Round::Ceil);
            let floor = scale.price_threshold(t, Round::Floor);
            for m in -20_000_i64..20_000 {
                let price = scale.price(m);
                let m = i128::from(m);
                assert_eq!(m < ceil, price < t, "{m} < {t}");
                assert_eq!(m <= floor, price <= t, "{m} <= {t}");
                assert_eq!(m > floor, price > t, "{m} > {t}");
                assert_eq!(m >= ceil, price >= t, "{m} >= {t}");
            }
        }
    }

    #[test]
    fn exact_mantissas_refuse_what_the_scale_cannot_hold() {
        let scale = scale("X", 2, 8);
        assert_eq!(
            scale.price_mantissa("95000.1".parse().unwrap()),
            Some(9_500_010)
        );
        assert_eq!(scale.price_mantissa("95000.105".parse().unwrap()), None);
        assert_eq!(
            scale.size_mantissa("0.0027".parse().unwrap()),
            Some(270_000)
        );
        assert_eq!(scale.size_mantissa("-1".parse().unwrap()), None);
        assert_eq!(scale.size_mantissa("0.000000001".parse().unwrap()), None);
        assert!(RecordScale::new(Symbol::from("X"), 19, 0).is_err());
    }

    #[test]
    fn pushback_hands_back_the_record_its_positioning_consumed() {
        let scale = scale("X", 0, 0);
        let trade = |ts| {
            Record::Trade(TradeRec {
                ts,
                px: 1,
                qty: 1,
                side: AggressorSide::Buyer,
            })
        };
        let river = MemoryRiver::new(scale, vec![trade(10), trade(20), trade(30)]);
        let mut positioned = Pushback::starting_at(river, Some(20));
        let read: Vec<u64> =
            std::iter::from_fn(|| positioned.next_record().map(|r| r.ts())).collect();
        assert_eq!(read, vec![20, 30]);
    }

    #[test]
    #[should_panic(expected = "mid-parent")]
    fn a_mid_parent_next_parent_is_refused() {
        let scale = scale("X", 0, 0);
        let trade = |ts| {
            Record::Trade(TradeRec {
                ts,
                px: 1,
                qty: 1,
                side: AggressorSide::Buyer,
            })
        };
        let mut parents = MemoryParents::new(scale, vec![vec![trade(1), trade(2)], vec![trade(3)]]);
        parents.next_record();
        parents.next_parent();
    }
}
