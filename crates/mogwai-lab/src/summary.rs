// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The `gen --type summary` accumulator: the MNQ TBBO fit's calibration
//! instrument, moved here from `mogwai-cli`'s `gen.rs` at phase 3b so the
//! protocol-11 fit driver can run its walks in-process instead of shelling
//! the `gen --type summary` subcommand the way the retired Python fit implementation did.
//!
//! `gen.rs` keeps its CLI surface unchanged and calls straight into
//! `summarize` here; `--type summary` output is byte-identical, which the
//! phase's move gate pins with a before/after comparison.
//!
//! This is the one module in `mogwai-lab` that depends on `mogwai-venue`:
//! an `InstrumentProfile` is the walk's instrument, and the fit resolves one
//! through the venue's own `Config::load` path exactly as the Python's
//! scratch-config walks did.

use mogwai_data::{ParentSource, Record, RecordScale};
use mogwai_protocol::AggressorSide;
use rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// Summary mode: the MNQ TBBO fit's calibration instrument. One JSON object of
// bounded sufficient statistics per run (one seed per invocation; the harness
// pools seeds). Every distributional field is a histogram or a
// count/sum/sum-of-squares accumulator, never a raw array - a simulated month
// is order 10^7 parents. Consumes the full record walk a parent at a time:
// every draw (sizes, prices, sides, quotes) is read as a compact record,
// which `advance_parent()`'s summary deliberately does not carry.
// ---------------------------------------------------------------------------

/// Signed-displacement bin width in ticks. Wrong-side observations land in
/// negative bins.
pub const DISPLACEMENT_BIN_TICKS: f64 = 0.05;

/// Fixed horizons (seconds) for the secondary realized-vol diagnostics.
pub const SUMMARY_VOL_HORIZONS_S: [u64; 2] = [60, 300];

const NS_PER_MINUTE: u64 = 60 * 1_000_000_000;

#[derive(Default, serde::Serialize)]
pub struct MomentAcc {
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

// -- Protocol-11 session-cell instrumentation (spec 4.5-4.7) ----------------

/// Robust-scale accumulator: one-maximum-trimmed mean absolute return derives
/// from these three fields, so no raw returns are stored.
#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct AbsCell {
    count: u64,
    sum_abs: f64,
    max_abs: f64,
}

impl AbsCell {
    fn push(&mut self, abs: f64) {
        self.count += 1;
        self.sum_abs += abs;
        if abs > self.max_abs {
            self.max_abs = abs;
        }
    }
}

/// Fixed-horizon accumulator under the shared segment-origin convention. The
/// signed moments serve the pooled RMS gates, the absolute pair the hourly
/// robust curves.
#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct HorizonCell {
    count: u64,
    sum: f64,
    sumsq: f64,
    sum_abs: f64,
    max_abs: f64,
}

impl HorizonCell {
    fn push(&mut self, r: f64) {
        self.count += 1;
        self.sum += r;
        self.sumsq += r * r;
        let abs = r.abs();
        self.sum_abs += abs;
        if abs > self.max_abs {
            self.max_abs = abs;
        }
    }
}

#[derive(serde::Serialize)]
pub struct GeneratedSessionCells {
    session_start_ns: u64,
    session_end_ns: u64,
    complete: bool,
    parent_count_by_hour: [u64; 24],
    mid_abs_by_hour: [AbsCell; 24],
    horizon_60_by_hour: [HorizonCell; 24],
    horizon_300_by_hour: [HorizonCell; 24],
}

impl GeneratedSessionCells {
    fn new(
        session_start_ns: u64,
        session_end_ns: u64,
        measured_from: u64,
        measured_until: u64,
    ) -> Self {
        Self {
            session_start_ns,
            session_end_ns,
            complete: session_start_ns >= measured_from && session_end_ns <= measured_until,
            parent_count_by_hour: [0; 24],
            mid_abs_by_hour: [AbsCell::default(); 24],
            horizon_60_by_hour: [HorizonCell::default(); 24],
            horizon_300_by_hour: [HorizonCell::default(); 24],
        }
    }
}

#[derive(serde::Serialize)]
pub struct TopMinuteRecord {
    pub minute_start_ns: u64,
    pub minute_end_ns: u64,
    pub utc_hour: u8,
    pub range_ticks: u64,
    pub parent_count: u64,
    pub trade_count: u64,
    pub low_price: String,
    pub high_price: String,
    pub trace_from_ns: u64,
    pub trace_until_ns: u64,
}

/// One open segment of a trading session, all bounds in UTC nanoseconds.
/// `session_start_ns` keys the session (the local 17:00 open instant);
/// `segment_origin_ns` anchors the fixed-horizon boundary grid (spec 4.6).
///
/// Named for the four bounds it carries rather than for the segment, because
/// [`crate::session::SessionSegment`] is the segment: this is that type's four
/// instants with the trade day and the segment name dropped. The two used to
/// share the name `SessionSegment` with identical field names on the shared
/// half, so a wrong import compiled and every field access kept working - the
/// error surfaced only where one of the two absent fields was read, arbitrarily
/// far from the import that caused it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionBounds {
    pub session_start_ns: u64,
    pub session_end_ns: u64,
    pub segment_origin_ns: u64,
    pub segment_end_ns: u64,
}

/// Maps an open instant to its session segment per spec 4.5: a local minute
/// at or past 17:00 opens the session dated the next civil day; one before
/// 15:15 belongs to the overnight segment of the prior day's open; the span
/// from 15:30 to 16:00 is the post-halt segment. Closed instants (halt,
/// break) return None - the calendar keeps generated events out of them, so
/// a None on a real event is a session-math defect, not a data case.
///
/// One implementation, in [`crate::session`]. This was a second copy of the
/// branch structure and of the four session-minute constants, kept honest by
/// `mogwai-cli`'s `session_segment_at_agrees_with_mogwai_lab` - a gate whose
/// own comment called it a temporary bridge until `mogwai-cli` was rewired
/// onto the lab. That rewire has since landed (this module is what `gen.rs`
/// calls), leaving a permanent two-copy gate with no external anchor: if the
/// session definition moved, both copies moved together and the gate stayed
/// green. The copy is gone rather than the gate strengthened, because one
/// implementation is what a two-copy gate is trying to approximate.
///
/// The two structs are not the same type and that is deliberate: the
/// [`crate::session`] one also reports the trade-date day number and the
/// segment name, which every caller here would have to ignore. The mapping
/// below is the whole difference.
pub fn session_segment_at(ts: u64, offset_minutes: i16) -> Option<SessionBounds> {
    crate::session::session_segment_at(ts, i32::from(offset_minutes)).map(SessionBounds::from)
}

impl From<crate::session::SessionSegment> for SessionBounds {
    fn from(seg: crate::session::SessionSegment) -> Self {
        Self {
            session_start_ns: seg.session_start_ns,
            session_end_ns: seg.session_end_ns,
            segment_origin_ns: seg.segment_origin_ns,
            segment_end_ns: seg.segment_end_ns,
        }
    }
}

/// `round(ask - bid) / tick` in whole ticks, as `Decimal` division and its
/// banker's rounding would give it, in integers: both prices and the tick are
/// mantissas at one scale. `u64::MAX` where the `Decimal` path's conversion
/// fails, which is a negative or unrepresentable width.
fn width_ticks(bid: i64, ask: i64, tick: i64) -> u64 {
    debug_assert!(tick > 0);
    let diff = i128::from(ask) - i128::from(bid);
    let tick = i128::from(tick);
    let quotient = diff.div_euclid(tick);
    let twice_rem = 2 * diff.rem_euclid(tick);
    let rounded = match twice_rem.cmp(&tick) {
        std::cmp::Ordering::Less => quotient,
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Equal => quotient + (quotient & 1),
    };
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

/// Integer-keyed counts rendered into the string-keyed histogram the summary
/// serializes. Two keys rendering to one string add together, so the result
/// is what counting by the string all along would have produced.
fn render_counts<K>(
    counts: std::collections::HashMap<K, u64>,
    render: impl Fn(K) -> String,
) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    for (key, count) in counts {
        *out.entry(render(key)).or_insert(0) += count;
    }
    out
}

pub fn utc_hour_of(ts: u64) -> usize {
    ((ts / 1_000_000_000) % 86_400 / 3_600) as usize
}

/// Per-horizon boundary state: (horizon_ns, next_k, prev_boundary_mid).
pub type HorizonState = Vec<(u64, u64, Option<f64>)>;

#[derive(serde::Serialize)]
pub struct SummaryAcc {
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
    // Per-minute trade-price ranges in integer ticks over minutes carrying
    // at least one in-window trade (the successor spec 3.3; the observed
    // side uses the identical convention). The per-seed maxima feed the
    // envelope gates seed by seed - never pooled.
    minute_range_ticks_hist: std::collections::BTreeMap<u64, u64>,
    minute_range_max_ticks: u64,
    minute_range_second_max_ticks: u64,
    horizon_vol: std::collections::BTreeMap<String, MomentAcc>,
    // Protocol-11 session cells (spec 4.5-4.6): one record per generated
    // session touched by the window, ascending session_start_ns. Deliberately
    // per session, not pooled per hour - the estimator gives each session one
    // vote. Empty when the profile carries no calendar: sessions are a
    // calendar construct, and the crypto presets have none.
    session_cells: Vec<GeneratedSessionCells>,
    // Protocol-11 worst-minute records (spec 4.7): the tail-location evidence
    // the protocol-12 spec consumes. Bounded at `TOP_MINUTE_RECORDS`.
    top_minutes: Vec<TopMinuteRecord>,
    first_book_mid: Option<String>,
    measured_from_ns: u64,
    measured_until_ns: u64,
}

/// The two work-size readings a benchmark row needs from a summary walk.
///
/// Accessors rather than public fields: every field here is a serialized leaf
/// of the fit's input record, and opening the struct up so a benchmark could
/// read two counts would make the whole accumulator writable from outside the
/// walk that owns it.
impl SummaryAcc {
    /// Parents the walk inferred over the measured window.
    #[must_use]
    pub fn parents(&self) -> u64 {
        self.parents
    }

    /// Sided trade rows the walk consumed over the measured window.
    #[must_use]
    pub fn sided_rows(&self) -> u64 {
        self.sided_rows
    }
}

/// Fixed capacity of the worst-minute collection (spec 4.7).
pub const TOP_MINUTE_RECORDS: usize = 32;

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

/// Per-minute trade-price ranges in integer ticks over the minutes carrying at
/// least one trade with its own timestamp in `[start, end)` - every such
/// trade, sided or not, booked or not, in a session or not. The successor
/// spec 3.3 convention, which the observed side shares; the summary and the
/// protocol 12b control both read it from here.
///
/// The arithmetic is the summary's own and is kept literally: low and high
/// each go through `price_f64`, the float difference is divided by the float
/// tick, then rounded and floored at zero. An exact integer division is a
/// different computation.
pub struct MinuteRangeAcc {
    start: u64,
    end: u64,
    tick_f: f64,
    /// (minute index, low, high mantissas) of the minute being accumulated.
    state: Option<(u64, i64, i64)>,
    hist: std::collections::BTreeMap<u64, u64>,
}

/// A finished [`MinuteRangeAcc`].
pub struct MinuteRanges {
    pub hist: std::collections::BTreeMap<u64, u64>,
    pub max_ticks: u64,
    /// The second largest range observed, not the second distinct value: a
    /// repeated maximum is its own second maximum.
    pub second_max_ticks: u64,
}

impl MinuteRangeAcc {
    #[must_use]
    pub fn new(start: u64, end: u64, tick_f: f64) -> Self {
        Self {
            start,
            end,
            tick_f,
            state: None,
            hist: Default::default(),
        }
    }

    /// One trade at its own timestamp; outside `[start, end)` it is ignored.
    /// Trades arrive in time order.
    pub fn push_trade(&mut self, ts: u64, price: i64, scale: &RecordScale) {
        if ts < self.start || ts >= self.end {
            return;
        }
        let minute = ts / NS_PER_MINUTE;
        match &mut self.state {
            Some((current, lo, hi)) if *current == minute => {
                *lo = (*lo).min(price);
                *hi = (*hi).max(price);
            }
            _ => {
                self.flush(scale);
                self.state = Some((minute, price, price));
            }
        }
    }

    fn flush(&mut self, scale: &RecordScale) {
        if let Some((_minute, lo, hi)) = self.state.take() {
            let (lo, hi) = (scale.price_f64(lo), scale.price_f64(hi));
            let range_ticks = ((hi - lo) / self.tick_f).round().max(0.0) as u64;
            *self.hist.entry(range_ticks).or_insert(0) += 1;
        }
    }

    /// Flush the last minute and read the two largest ranges.
    #[must_use]
    pub fn finish(mut self, scale: &RecordScale) -> MinuteRanges {
        self.flush(scale);
        let mut max_ticks = 0;
        let mut second_max_ticks = 0;
        let mut ranges = self.hist.iter().rev();
        if let Some((&largest, &count)) = ranges.next() {
            max_ticks = largest;
            second_max_ticks = if count >= 2 {
                largest
            } else {
                ranges.next().map_or(0, |(&next, _)| next)
            };
        }
        MinuteRanges {
            hist: self.hist,
            max_ticks,
            second_max_ticks,
        }
    }
}

/// A per-minute map written through the minute last written. A write to
/// another minute merges the held one into the map first, so a revisited
/// minute combines with what it held rather than replacing it, and the map
/// comes out exactly as writing every update straight into it would leave it.
struct MinuteMap<V> {
    map: std::collections::BTreeMap<u64, V>,
    current: Option<(u64, V)>,
    merge: fn(&mut V, V),
}

impl<V: Copy> MinuteMap<V> {
    fn new(merge: fn(&mut V, V)) -> Self {
        Self {
            map: std::collections::BTreeMap::new(),
            current: None,
            merge,
        }
    }

    /// Merge `value` into `minute`'s, or start it there.
    fn update(&mut self, minute: u64, value: V) {
        match &mut self.current {
            Some((held, current)) if *held == minute => (self.merge)(current, value),
            _ => {
                self.flush();
                self.current = Some((minute, value));
            }
        }
    }

    fn flush(&mut self) {
        if let Some((minute, value)) = self.current.take() {
            match self.map.get_mut(&minute) {
                Some(held) => (self.merge)(held, value),
                None => {
                    self.map.insert(minute, value);
                }
            }
        }
    }

    fn finish(mut self) -> std::collections::BTreeMap<u64, V> {
        self.flush();
        self.map
    }
}

fn decimal_key(d: Decimal) -> String {
    d.normalize().to_string()
}

/// A displacement's bin, as the bits of its floored lower edge.
fn displacement_bin(d_ticks: f64) -> u64 {
    ((d_ticks / DISPLACEMENT_BIN_TICKS).floor() * DISPLACEMENT_BIN_TICKS).to_bits()
}

/// A bin's serialized key.
fn displacement_key(bin: u64) -> String {
    format!("{:.2}", f64::from_bits(bin))
}

/// The per-parent histograms `summarize` counts by integer key.
struct ParentCounts<'a> {
    bid_sizes: &'a mut std::collections::HashMap<u64, u64>,
    ask_sizes: &'a mut std::collections::HashMap<u64, u64>,
    buyer: &'a mut std::collections::HashMap<u64, u64>,
    seller: &'a mut std::collections::HashMap<u64, u64>,
}

/// One parent being folded: the book in force at its first trade and what its
/// children have printed so far. Parents are delimited by the source's own
/// `next_parent`, never inferred from quote emissions: the discrete-book path
/// can publish two quotes for one parent or none. Prices and sizes are record
/// mantissas at the river's scale.
struct OpenParent {
    quote_mid: f64,
    width_ticks: u64,
    bid_sz: u64,
    ask_sz: u64,
    first_ts: u64,
    first_price: i64,
    first_side: AggressorSide,
    rows: u64,
    /// Distinct trade prices so far; the prices themselves are in the
    /// walk's reused `levels` buffer.
    level_count: u64,
}

/// Fold the river into the summary, one whole parent at a time.
///
/// Parent statistics count a parent exactly when its first trade's timestamp
/// lies in `[start, end)`, and then count the whole parent - every trade
/// child, including one that prints at or after `end`. Per-record statistics
/// (sizes, minute ranges, quote and horizon state) are selected per record:
/// trades by their own timestamp in `[start, end)`, quotes by `ts < end`.
/// The two selections are deliberately separate, because a parent can
/// straddle `end` or a minute boundary. A parent with no trade, or whose
/// first trade precedes every quote of the walk, has no book to attribute
/// and counts as no parent. A burn-in walk before `start` is consumed and
/// discarded. The source must already sit at a parent boundary at its walk
/// start (possibly `start - burn_in`).
pub fn summarize(
    source: &mut dyn ParentSource,
    profile: &mogwai_venue::source::InstrumentProfile,
    seed: u64,
    start: u64,
    end: u64,
) -> SummaryAcc {
    let tick = profile.scalars.modal_tick;
    let tick_f = f64::try_from(tick).unwrap_or(f64::NAN);
    let calendar = profile.calendar.as_ref();
    // The scale is fixed for the river's life; held by value because each
    // parent view borrows the source.
    let scale: RecordScale = source.scale().clone();

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
        minute_range_ticks_hist: Default::default(),
        minute_range_max_ticks: 0,
        minute_range_second_max_ticks: 0,
        horizon_vol: Default::default(),
        session_cells: Vec::new(),
        top_minutes: Vec::new(),
        first_book_mid: None,
        measured_from_ns: start,
        measured_until_ns: end,
    };

    // Protocol-11 session-cell state. Sessions exist only under a calendar;
    // the offset is the calendar's own, and the in-session structure is the
    // fixed CME shape (see session_segment_at).
    let mut segments =
        calendar.map(|c| crate::session::SegmentCursor::new(i32::from(c.utc_offset_minutes)));
    let mut sessions: std::collections::BTreeMap<u64, GeneratedSessionCells> = Default::default();
    // The tick as a price mantissa, for integer quote widths; `None` keeps
    // the `Decimal` division for a tick the price scale cannot hold.
    let tick_mantissa = scale.price_mantissa(tick).filter(|m| *m > 0);
    // Histograms counted by integer key and rendered to their serialized
    // string keys once, at the end: sizes by quantity mantissa,
    // displacements by the floored bin's bits.
    let mut size_counts: std::collections::HashMap<u64, u64> = Default::default();
    let mut bid_size_counts: std::collections::HashMap<u64, u64> = Default::default();
    let mut ask_size_counts: std::collections::HashMap<u64, u64> = Default::default();
    let mut buyer_displacement_counts: std::collections::HashMap<u64, u64> = Default::default();
    let mut seller_displacement_counts: std::collections::HashMap<u64, u64> = Default::default();
    // Moments per fixed horizon, in `SUMMARY_VOL_HORIZONS_S` order, keyed to
    // the serialized horizon names at the end.
    let mut horizon_moments: Vec<MomentAcc> = SUMMARY_VOL_HORIZONS_S
        .iter()
        .map(|_| MomentAcc::default())
        .collect();
    // The distinct trade levels of the parent being folded, reused across
    // parents.
    let mut levels: Vec<i64> = Vec::new();
    // Adjacent-parent valid-mid chain, keyed by segment origin so a segment
    // transition breaks it without extra bookkeeping (spec 4.1).
    let mut prev_vol_parent: Option<(u64, f64)> = None;
    // The active segment's fixed-horizon boundary state: per horizon,
    // (horizon_ns, next_k, prev_boundary_mid), plus the segment-local
    // as-of mid. Boundaries are segment_origin + k * horizon, k >= 1,
    // strictly inside the segment (spec 4.6); the first boundary having
    // an as-of mid establishes. The as-of is deliberately never the global
    // last_mid: state is independent per segment (rule 1), so a pre-halt
    // quote must never establish or price a post-halt boundary - the
    // as-of resets to None on every segment change, exactly as the
    // observed harness resets its chain.
    let mut seg_state: Option<(SessionBounds, HorizonState, Option<f64>)> = None;
    // Worst-minute detail: minute index -> (low, high, trade_count) on the
    // exact price grid as record mantissas, plus parents by first-child
    // minute (spec 4.7). Each is fronted by the minute being written, merged
    // in when another minute is written; trades and parents arrive in time
    // order, so that is nearly every write.
    let mut minute_detail = MinuteMap::<(i64, i64, u64)>::new(|held, (lo, hi, count)| {
        held.0 = held.0.min(lo);
        held.1 = held.1.max(hi);
        held.2 += count;
    });
    let mut minute_parents = MinuteMap::<u64>::new(|held, count| *held += count);

    // Advance the active segment's boundaries up to `until_ts` - exclusive
    // during the walk (a boundary equal to an event timestamp waits for a
    // later event, so equal-timestamp quotes update the as-of mid first -
    // spec 4.6 rule 6), inclusive at a flush. `as_of` is the last valid
    // quote mid seen so far; `window` is (measured_from, measured_until).
    fn advance_segment_boundaries(
        sessions: &mut std::collections::BTreeMap<u64, GeneratedSessionCells>,
        seg: &SessionBounds,
        horizons: &mut HorizonState,
        until_ts: u64,
        inclusive: bool,
        as_of: Option<f64>,
        window: (u64, u64),
    ) {
        let (measured_from, measured_until) = window;
        for (h_ns, next_k, prev_boundary_mid) in horizons.iter_mut() {
            loop {
                let boundary = seg.segment_origin_ns.saturating_add(*next_k * *h_ns);
                if boundary >= seg.segment_end_ns {
                    break;
                }
                if if inclusive {
                    boundary > until_ts
                } else {
                    boundary >= until_ts
                } {
                    break;
                }
                match (*prev_boundary_mid, as_of) {
                    (Some(prev), Some(cur)) if prev > 0.0 && cur > 0.0 => {
                        let window_start = boundary - *h_ns;
                        // Rule 7: no return crosses a UTC hour boundary.
                        if utc_hour_of(window_start) == utc_hour_of(boundary) {
                            let hour = utc_hour_of(boundary);
                            let cell = sessions.entry(seg.session_start_ns).or_insert_with(|| {
                                GeneratedSessionCells::new(
                                    seg.session_start_ns,
                                    seg.session_end_ns,
                                    measured_from,
                                    measured_until,
                                )
                            });
                            let target = if *h_ns == 60_000_000_000 {
                                &mut cell.horizon_60_by_hour[hour]
                            } else {
                                &mut cell.horizon_300_by_hour[hour]
                            };
                            target.push((cur / prev).ln());
                        }
                        *prev_boundary_mid = as_of;
                    }
                    // Rule 4: the first boundary having an as-of mid
                    // establishes and emits nothing; a boundary before any
                    // quote neither establishes nor emits.
                    (None, Some(_)) => *prev_boundary_mid = as_of,
                    _ => {}
                }
                *next_k += 1;
            }
        }
    }

    // As-of state for the fixed-horizon diagnostics: per horizon, the index of
    // the next boundary and the as-of mid at the previous boundary.
    let mut horizon_state: Vec<(u64, u64, Option<f64>)> = SUMMARY_VOL_HORIZONS_S
        .iter()
        .map(|h| (h * 1_000_000_000, 1, None))
        .collect();
    let mut last_mid: Option<f64> = None;
    // The as-of mid AT `start`, frozen when the first post-start quote
    // arrives: the first boundary's window opens at `start`, and a burn-in
    // quote at or before it is its legitimate as-of observation. The flag
    // marks the freeze, because the frozen value is legitimately None when
    // no quote precedes `start` - an Option's is_none cannot distinguish
    // not-yet-frozen from frozen-empty and would re-freeze every quote.
    let mut asof_start: Option<f64> = None;
    let mut asof_start_frozen = false;

    // (mid, width_ticks, bid_sz mantissa, ask_sz mantissa) of the last quote.
    let mut pending_quote: Option<(f64, u64, u64, u64)> = None;
    let mut prev_parent: Option<(u64, f64)> = None; // (first_ts, quote_mid)
    let mut minute_ranges = MinuteRangeAcc::new(start, end, tick_f);

    let finalize = |acc: &mut SummaryAcc,
                    prev: &mut Option<(u64, f64)>,
                    sessions: &mut std::collections::BTreeMap<u64, GeneratedSessionCells>,
                    prev_vol: &mut Option<(u64, f64)>,
                    minute_parents: &mut MinuteMap<u64>,
                    segments: &mut Option<crate::session::SegmentCursor>,
                    counts: &mut ParentCounts<'_>,
                    parent: OpenParent| {
        if parent.first_ts < start || parent.first_ts >= end {
            return;
        }
        // Protocol-11 session cells: parent count by endpoint hour, and the
        // adjacent valid-mid robust-scale chain within the segment (spec 4.1;
        // the chain key is the segment origin, so a segment transition breaks
        // it and an out-of-segment predecessor never contributes).
        if let Some(cursor) = segments
            && let Some(seg) = cursor.at(parent.first_ts).map(SessionBounds::from)
        {
            let hour = utc_hour_of(parent.first_ts);
            let cell = sessions.entry(seg.session_start_ns).or_insert_with(|| {
                GeneratedSessionCells::new(seg.session_start_ns, seg.session_end_ns, start, end)
            });
            cell.parent_count_by_hour[hour] += 1;
            if parent.quote_mid.is_finite() && parent.quote_mid > 0.0 {
                if let Some((prev_origin, prev_mid)) = *prev_vol
                    && prev_origin == seg.segment_origin_ns
                {
                    let r = (parent.quote_mid / prev_mid).ln();
                    cell.mid_abs_by_hour[hour].push(r.abs());
                }
                *prev_vol = Some((seg.segment_origin_ns, parent.quote_mid));
            }
        }
        minute_parents.update(parent.first_ts / 60_000_000_000, 1);
        acc.parents += 1;
        acc.sided_rows += parent.rows;
        if parent.rows == 1 {
            acc.single_parents += 1;
        }
        acc.level_count_sum += parent.level_count;
        *acc.width_ticks_histogram
            .entry(parent.width_ticks)
            .or_insert(0) += 1;
        *counts.bid_sizes.entry(parent.bid_sz).or_insert(0) += 1;
        *counts.ask_sizes.entry(parent.ask_sz).or_insert(0) += 1;
        if parent.quote_mid.is_finite() && tick_f.is_finite() {
            let first = scale.price_f64(parent.first_price);
            let raw = (first - parent.quote_mid) / tick_f;
            match parent.first_side {
                AggressorSide::Buyer => {
                    *counts.buyer.entry(displacement_bin(raw)).or_insert(0) += 1;
                }
                AggressorSide::Seller => {
                    *counts.seller.entry(displacement_bin(-raw)).or_insert(0) += 1;
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

    while let Some(view) = source.next_parent() {
        // Records are in time order, so a parent whose first record is at or
        // past `end` holds nothing any selection below admits, and neither
        // does any later parent.
        if view.records[0].ts() >= end {
            break;
        }
        let mut open: Option<OpenParent> = None;
        // Set at the parent's first trade, attributed or not: a parent whose
        // first trade had no book is unattributed as a whole, never reopened
        // as a fragment at a later child.
        let mut seen_trade = false;
        for record in view.records {
            let ts = record.ts();
            match *record {
                Record::Quote(q) => {
                    // A quote at or past `end` must not move the as-of mid the
                    // closing flush reads.
                    if ts >= end {
                        continue;
                    }
                    // Protocol-11 segment horizon state: settle the outgoing
                    // segment's remaining boundaries before switching, then
                    // advance the active one strictly past-boundary (rule 6).
                    let quote_seg = segments
                        .as_mut()
                        .and_then(|cursor| cursor.at(ts))
                        .map(SessionBounds::from);
                    if let Some(seg) = quote_seg {
                        let switch = match &seg_state {
                            Some((current, _, _)) => *current != seg,
                            None => true,
                        };
                        if switch {
                            if let Some((old_seg, mut horizons, old_mid)) = seg_state.take() {
                                advance_segment_boundaries(
                                    &mut sessions,
                                    &old_seg,
                                    &mut horizons,
                                    old_seg.segment_end_ns,
                                    true,
                                    old_mid,
                                    (start, end),
                                );
                            }
                            seg_state = Some((
                                seg,
                                SUMMARY_VOL_HORIZONS_S
                                    .iter()
                                    .map(|h| (h * 1_000_000_000, 1, None))
                                    .collect(),
                                None,
                            ));
                        }
                        if let Some((current, horizons, seg_mid)) = &mut seg_state {
                            advance_segment_boundaries(
                                &mut sessions,
                                current,
                                horizons,
                                ts,
                                false,
                                *seg_mid,
                                (start, end),
                            );
                        }
                    }
                    let mid = (scale.price_f64(q.bid_px) + scale.price_f64(q.ask_px)) / 2.0;
                    let width = match tick_mantissa {
                        Some(tick) => width_ticks(q.bid_px, q.ask_px, tick),
                        None => ((scale.price(q.ask_px) - scale.price(q.bid_px)) / tick)
                            .round()
                            .try_into()
                            .unwrap_or(u64::MAX),
                    };
                    pending_quote = Some((mid, width, q.bid_qty, q.ask_qty));
                    if ts >= start && acc.first_book_mid.is_none() {
                        let (bid_px, ask_px) = (scale.price(q.bid_px), scale.price(q.ask_px));
                        acc.first_book_mid = Some(decimal_key((bid_px + ask_px) / Decimal::TWO));
                    }
                    // The fixed-horizon as-of state advances on quotes only: the
                    // mid IS the quote mid, and a boundary takes the last mid at
                    // or before it.
                    if ts > start && !asof_start_frozen {
                        asof_start = last_mid;
                        asof_start_frozen = true;
                    }
                    for ((h_ns, next_k, prev_boundary_mid), moments) in
                        horizon_state.iter_mut().zip(&mut horizon_moments)
                    {
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
                                moments.push((cur / prev).ln());
                            }
                            *prev_boundary_mid = last_mid;
                            *next_k += 1;
                        }
                    }
                    last_mid = Some(mid);
                    // Segment-local as-of: only a quote inside the active
                    // segment feeds it - never one from closed time or another
                    // segment (rule 1's independence).
                    if let Some((current, _, seg_mid)) = &mut seg_state
                        && quote_seg.as_ref() == Some(current)
                    {
                        *seg_mid = Some(mid);
                    }
                }
                Record::Trade(t) => {
                    minute_ranges.push_trade(t.ts, t.px, &scale);
                    if t.ts >= start && t.ts < end {
                        *size_counts.entry(t.qty).or_insert(0) += 1;
                        // Protocol-11 worst-minute detail on the exact grid: the
                        // records serialize decimal prices and an exact integer
                        // tick range, where the legacy histogram rounds in f64.
                        minute_detail.update(t.ts / 60_000_000_000, (t.px, t.px, 1));
                    }
                    match &mut open {
                        Some(parent) => {
                            parent.rows += 1;
                            if !levels.contains(&t.px) {
                                levels.push(t.px);
                                parent.level_count += 1;
                            }
                        }
                        None if !seen_trade => {
                            // The book in force at the parent's first trade: the
                            // last quote at or before it, in this parent or an
                            // earlier one. None before the walk's first quote.
                            if let Some((mid, width, bid_sz, ask_sz)) = pending_quote {
                                levels.clear();
                                levels.push(t.px);
                                open = Some(OpenParent {
                                    quote_mid: mid,
                                    width_ticks: width,
                                    bid_sz,
                                    ask_sz,
                                    first_ts: t.ts,
                                    first_price: t.px,
                                    first_side: t.side,
                                    rows: 1,
                                    level_count: 1,
                                });
                            }
                        }
                        None => {}
                    }
                    seen_trade = true;
                }
            }
        }
        if let Some(parent) = open.take() {
            finalize(
                &mut acc,
                &mut prev_parent,
                &mut sessions,
                &mut prev_vol_parent,
                &mut minute_parents,
                &mut segments,
                &mut ParentCounts {
                    bid_sizes: &mut bid_size_counts,
                    ask_sizes: &mut ask_size_counts,
                    buyer: &mut buyer_displacement_counts,
                    seller: &mut seller_displacement_counts,
                },
                parent,
            );
        }
    }
    // Settle the active segment's remaining boundaries: to the segment end
    // for a segment the window fully covers, to `end` (inclusive, matching
    // the legacy flush) for one the window cuts short. The as-of mid is the
    // final last_mid - no later quote exists in the walk.
    if let Some((seg, mut horizons, seg_mid)) = seg_state.take() {
        let until = seg.segment_end_ns.min(end);
        advance_segment_boundaries(
            &mut sessions,
            &seg,
            &mut horizons,
            until,
            true,
            seg_mid,
            (start, end),
        );
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
    for ((h_ns, next_k, prev_boundary_mid), moments) in
        horizon_state.iter_mut().zip(&mut horizon_moments)
    {
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
                moments.push((cur / prev).ln());
            }
            *prev_boundary_mid = last_mid;
            *next_k += 1;
        }
    }
    acc.horizon_vol = SUMMARY_VOL_HORIZONS_S
        .iter()
        .map(ToString::to_string)
        .zip(horizon_moments)
        .collect();
    let size_key = |qty: u64| decimal_key(scale.size(qty));
    acc.size_histogram = render_counts(size_counts, size_key);
    acc.bid_size_histogram = render_counts(bid_size_counts, size_key);
    acc.ask_size_histogram = render_counts(ask_size_counts, size_key);
    acc.buyer_displacement_hist = render_counts(buyer_displacement_counts, displacement_key);
    acc.seller_displacement_hist = render_counts(seller_displacement_counts, displacement_key);
    let ranges = minute_ranges.finish(&scale);
    acc.minute_range_ticks_hist = ranges.hist;
    acc.minute_range_max_ticks = ranges.max_ticks;
    acc.minute_range_second_max_ticks = ranges.second_max_ticks;
    let minute_detail: std::collections::BTreeMap<u64, (Decimal, Decimal, u64)> = minute_detail
        .finish()
        .into_iter()
        .map(|(minute, (lo, hi, count))| (minute, (scale.price(lo), scale.price(hi), count)))
        .collect();
    acc.top_minutes = rank_top_minutes(&minute_detail, &minute_parents.finish(), tick);
    acc.session_cells = sessions.into_values().collect();
    acc
}

/// Protocol-11 worst-minute records (spec 4.7): every populated minute,
/// ordered by range descending then minute ascending, truncated to
/// capacity; repeated equal ranges occupy distinct entries. The range is
/// exact integer ticks on the price grid - a nonintegral range is an
/// off-grid print and refuses.
pub fn rank_top_minutes(
    minute_detail: &std::collections::BTreeMap<u64, (Decimal, Decimal, u64)>,
    minute_parents: &std::collections::BTreeMap<u64, u64>,
    tick: Decimal,
) -> Vec<TopMinuteRecord> {
    let mut minute_records: Vec<(u64, u64)> = minute_detail
        .iter()
        .map(|(&minute, &(lo, hi, _))| {
            let ticks = (hi - lo) / tick;
            let rounded = ticks.round();
            assert!(
                ticks == rounded,
                "nonintegral minute range: {lo} .. {hi} on tick {tick}"
            );
            let range: u64 = rounded.try_into().unwrap_or(u64::MAX);
            (range, minute)
        })
        .collect();
    // Every range above was validated before any is dropped. Minutes are
    // distinct, so the order is total and the selected set is unambiguous
    // even through ties at the cut.
    let order = |a: &(u64, u64), b: &(u64, u64)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
    if minute_records.len() > TOP_MINUTE_RECORDS {
        minute_records.select_nth_unstable_by(TOP_MINUTE_RECORDS, order);
        minute_records.truncate(TOP_MINUTE_RECORDS);
    }
    minute_records.sort_unstable_by(order);
    minute_records
        .into_iter()
        .map(|(range_ticks, minute)| {
            let (lo, hi, trade_count) = minute_detail[&minute];
            let minute_start_ns = minute * 60_000_000_000;
            let minute_end_ns = minute_start_ns + 60_000_000_000;
            TopMinuteRecord {
                minute_start_ns,
                minute_end_ns,
                utc_hour: utc_hour_of(minute_start_ns) as u8,
                range_ticks,
                parent_count: minute_parents.get(&minute).copied().unwrap_or(0),
                trade_count,
                low_price: decimal_key(lo),
                high_price: decimal_key(hi),
                trace_from_ns: minute_start_ns,
                trace_until_ns: minute_end_ns,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_data::{MemoryParents, QuoteRec, TradeRec};

    /// 2026-07-06T23:00Z, inside the July 7 MNQ session.
    const T0: u64 = 1_783_378_800_000_000_000;
    const S: u64 = 1_000_000_000;

    fn px(level: i64) -> i64 {
        2_300_000 + 25 * level
    }

    fn quote(ts: u64, level: i64) -> Record {
        Record::Quote(QuoteRec {
            ts,
            bid_px: px(level - 1),
            ask_px: px(level + 1),
            bid_qty: 1,
            ask_qty: 1,
        })
    }

    fn trade(ts: u64, level: i64, qty: u64) -> Record {
        Record::Trade(TradeRec {
            ts,
            px: px(level),
            qty,
            side: AggressorSide::Buyer,
        })
    }

    /// The integer width is the `Decimal` division and banker's rounding it
    /// replaced, for every width up to 40 ticks in both directions on ticks
    /// of one to seven mantissa units - odd ticks give exact half-tick ties,
    /// both parities of quotient included - and for an inverted book.
    #[test]
    fn integer_quote_widths_round_as_the_decimal_path_did() {
        let scale = RecordScale::new(mogwai_protocol::Symbol::from("X"), 2, 0).expect("scale");
        for tick_units in 1_i64..=7 {
            let tick = scale.price(tick_units);
            for bid in [0_i64, 1, 2_300_000] {
                for spread in -40 * tick_units..=40 * tick_units {
                    let ask = bid + spread;
                    let want: u64 = ((scale.price(ask) - scale.price(bid)) / tick)
                        .round()
                        .try_into()
                        .unwrap_or(u64::MAX);
                    assert_eq!(
                        width_ticks(bid, ask, tick_units),
                        want,
                        "bid {bid} ask {ask} tick {tick_units}"
                    );
                }
            }
        }
    }

    /// Top-minute selection keeps exactly the first 32 of the full sort
    /// (range descending, then minute ascending) at every size around the
    /// cut and through a tie that straddles it, and still refuses an off-grid
    /// range that would not have been selected.
    #[test]
    fn top_minutes_select_what_a_full_sort_keeps() {
        let tick = Decimal::new(25, 2);
        let full_sort = |detail: &std::collections::BTreeMap<u64, (Decimal, Decimal, u64)>| {
            let mut rows: Vec<(u64, u64)> = detail
                .iter()
                .map(|(&m, &(lo, hi, _))| (((hi - lo) / tick).try_into().expect("ticks"), m))
                .collect();
            rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            rows.truncate(TOP_MINUTE_RECORDS);
            rows
        };
        let ranges: Vec<i64> = (0..80).map(|i| (i * 37) % 11).collect();
        for len in [0, 31, 32, 33, 80] {
            let detail: std::collections::BTreeMap<u64, (Decimal, Decimal, u64)> = ranges[..len]
                .iter()
                .enumerate()
                .map(|(i, &r)| {
                    let lo = Decimal::new(23_000, 0);
                    (
                        1_000 + 7 * i as u64 % 97,
                        (lo, lo + tick * Decimal::from(r), 1),
                    )
                })
                .collect();
            let got: Vec<(u64, u64)> = rank_top_minutes(&detail, &Default::default(), tick)
                .iter()
                .map(|rec| (rec.range_ticks, rec.minute_start_ns / 60_000_000_000))
                .collect();
            assert_eq!(got, full_sort(&detail), "{len} minutes");
        }
        let mut off_grid: std::collections::BTreeMap<u64, (Decimal, Decimal, u64)> = (0..40)
            .map(|m| (m, (Decimal::ZERO, tick * Decimal::from(50), 1)))
            .collect();
        off_grid.insert(99, (Decimal::ZERO, Decimal::new(1, 2), 1));
        let refused =
            std::panic::catch_unwind(|| rank_top_minutes(&off_grid, &Default::default(), tick));
        assert!(
            refused.is_err(),
            "an unselected off-grid minute must still refuse"
        );
    }

    /// A minute written again after another merges into what it held.
    #[test]
    fn a_minute_map_merges_a_revisited_minute() {
        let mut map = MinuteMap::<(i64, i64, u64)>::new(|held, (lo, hi, n)| {
            held.0 = held.0.min(lo);
            held.1 = held.1.max(hi);
            held.2 += n;
        });
        map.update(5, (10, 10, 1));
        map.update(5, (12, 12, 1));
        map.update(6, (3, 3, 1));
        map.update(5, (8, 8, 1));
        map.update(6, (9, 9, 1));
        let done = map.finish();
        assert_eq!(done[&5], (8, 12, 3));
        assert_eq!(done[&6], (3, 9, 2));
        assert_eq!(done.len(), 2);
    }

    /// Parent statistics follow the source's divisions, and the per-record
    /// window stays separate from parent selection. Quote inference would
    /// read four parents here - the two quoteless parents merged into the
    /// first, the two-quote parent split - where the source divides five;
    /// the straddling parent counts both its trades as rows while its
    /// post-window print stays out of the size histogram.
    #[test]
    fn parents_come_from_the_source_and_the_record_window_is_separate() {
        let profile = mogwai_venue::config::profile_from_preset("MNQ").expect("MNQ preset");
        let end = T0 + 3 * S;
        let scale = RecordScale::new(mogwai_protocol::Symbol::from("MNQ"), 2, 0).expect("scale");
        let mut source = MemoryParents::new(
            scale,
            vec![
                vec![quote(T0, 0), trade(T0, 0, 1)],
                vec![trade(T0 + S, 0, 1)],
                vec![trade(T0 + 3 * S / 2, 0, 1)],
                vec![
                    quote(T0 + 2 * S, 4),
                    trade(T0 + 2 * S, 4, 1),
                    quote(T0 + 2 * S + 1, 8),
                    trade(T0 + 2 * S + 1, 8, 1),
                ],
                vec![
                    quote(T0 + 5 * S / 2, 0),
                    trade(T0 + 5 * S / 2, 0, 1),
                    trade(end + 1, 0, 7),
                ],
                vec![quote(end, 0), trade(end, 0, 1)],
            ],
        );
        let acc = summarize(&mut source, &profile, 1, T0, end);
        assert_eq!(acc.parents(), 5);
        assert_eq!(acc.sided_rows(), 7);
        let value = serde_json::to_value(&acc).expect("serialize");
        let sizes = value["size_histogram"].as_object().expect("size_histogram");
        assert_eq!(sizes.get("1").and_then(serde_json::Value::as_u64), Some(6));
        assert!(
            !sizes.contains_key("7"),
            "a print at or past `end` entered the record window: {sizes:?}"
        );
    }
}
