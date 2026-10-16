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
    crate::session::session_segment_at(ts, i32::from(offset_minutes)).map(|seg| SessionBounds {
        session_start_ns: seg.session_start_ns,
        session_end_ns: seg.session_end_ns,
        segment_origin_ns: seg.segment_origin_ns,
        segment_end_ns: seg.segment_end_ns,
    })
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

fn decimal_key(d: Decimal) -> String {
    d.normalize().to_string()
}

fn displacement_key(d_ticks: f64) -> String {
    let bin = (d_ticks / DISPLACEMENT_BIN_TICKS).floor() * DISPLACEMENT_BIN_TICKS;
    format!("{bin:.2}")
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
    levels: Vec<i64>,
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
        horizon_vol: SUMMARY_VOL_HORIZONS_S
            .iter()
            .map(|h| (h.to_string(), MomentAcc::default()))
            .collect(),
        session_cells: Vec::new(),
        top_minutes: Vec::new(),
        first_book_mid: None,
        measured_from_ns: start,
        measured_until_ns: end,
    };

    // Protocol-11 session-cell state. Sessions exist only under a calendar;
    // the offset is the calendar's own, and the in-session structure is the
    // fixed CME shape (see session_segment_at).
    let cal_offset = calendar.map(|c| c.utc_offset_minutes);
    let mut sessions: std::collections::BTreeMap<u64, GeneratedSessionCells> = Default::default();
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
    // minute (spec 4.7).
    let mut minute_detail: std::collections::BTreeMap<u64, (i64, i64, u64)> = Default::default();
    let mut minute_parents: std::collections::BTreeMap<u64, u64> = Default::default();

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
    // (minute index, low, high mantissas) of the in-window minute being
    // accumulated.
    let mut minute_state: Option<(u64, i64, i64)> = None;

    fn flush_minute(
        acc: &mut SummaryAcc,
        state: &mut Option<(u64, i64, i64)>,
        scale: &RecordScale,
        tick_f: f64,
    ) {
        if let Some((_minute, lo, hi)) = state.take() {
            let (lo, hi) = (scale.price_f64(lo), scale.price_f64(hi));
            let range_ticks = ((hi - lo) / tick_f).round().max(0.0) as u64;
            *acc.minute_range_ticks_hist.entry(range_ticks).or_insert(0) += 1;
        }
    }

    let finalize = |acc: &mut SummaryAcc,
                    prev: &mut Option<(u64, f64)>,
                    sessions: &mut std::collections::BTreeMap<u64, GeneratedSessionCells>,
                    prev_vol: &mut Option<(u64, f64)>,
                    minute_parents: &mut std::collections::BTreeMap<u64, u64>,
                    parent: OpenParent| {
        if parent.first_ts < start || parent.first_ts >= end {
            return;
        }
        // Protocol-11 session cells: parent count by endpoint hour, and the
        // adjacent valid-mid robust-scale chain within the segment (spec 4.1;
        // the chain key is the segment origin, so a segment transition breaks
        // it and an out-of-segment predecessor never contributes).
        if let Some(offset) = cal_offset
            && let Some(seg) = session_segment_at(parent.first_ts, offset)
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
        *minute_parents
            .entry(parent.first_ts / 60_000_000_000)
            .or_insert(0) += 1;
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
            .entry(decimal_key(scale.size(parent.bid_sz)))
            .or_insert(0) += 1;
        *acc.ask_size_histogram
            .entry(decimal_key(scale.size(parent.ask_sz)))
            .or_insert(0) += 1;
        if parent.quote_mid.is_finite() && tick_f.is_finite() {
            let first = scale.price_f64(parent.first_price);
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
                    if let Some(offset) = cal_offset
                        && let Some(seg) = session_segment_at(ts, offset)
                    {
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
                    let (bid_px, ask_px) = (scale.price(q.bid_px), scale.price(q.ask_px));
                    let width = ((ask_px - bid_px) / tick)
                        .round()
                        .try_into()
                        .unwrap_or(u64::MAX);
                    pending_quote = Some((mid, width, q.bid_qty, q.ask_qty));
                    if ts >= start && acc.first_book_mid.is_none() {
                        acc.first_book_mid = Some(decimal_key((bid_px + ask_px) / Decimal::TWO));
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
                    // Segment-local as-of: only a quote inside the active
                    // segment feeds it - never one from closed time or another
                    // segment (rule 1's independence).
                    if let Some(offset) = cal_offset
                        && let Some((current, _, seg_mid)) = &mut seg_state
                        && session_segment_at(ts, offset).as_ref() == Some(current)
                    {
                        *seg_mid = Some(mid);
                    }
                }
                Record::Trade(t) => {
                    if t.ts >= start && t.ts < end {
                        *acc.size_histogram
                            .entry(decimal_key(scale.size(t.qty)))
                            .or_insert(0) += 1;
                        let minute = t.ts / 60_000_000_000;
                        let price = t.px;
                        match &mut minute_state {
                            Some((current, lo, hi)) if *current == minute => {
                                *lo = (*lo).min(price);
                                *hi = (*hi).max(price);
                            }
                            _ => {
                                flush_minute(&mut acc, &mut minute_state, &scale, tick_f);
                                minute_state = Some((minute, price, price));
                            }
                        }
                        // Protocol-11 worst-minute detail on the exact grid: the
                        // records serialize decimal prices and an exact integer
                        // tick range, where the legacy histogram rounds in f64.
                        minute_detail
                            .entry(minute)
                            .and_modify(|(lo, hi, count)| {
                                if t.px < *lo {
                                    *lo = t.px;
                                }
                                if t.px > *hi {
                                    *hi = t.px;
                                }
                                *count += 1;
                            })
                            .or_insert((t.px, t.px, 1));
                    }
                    match &mut open {
                        Some(parent) => {
                            parent.rows += 1;
                            if !parent.levels.contains(&t.px) {
                                parent.levels.push(t.px);
                            }
                        }
                        None if !seen_trade => {
                            // The book in force at the parent's first trade: the
                            // last quote at or before it, in this parent or an
                            // earlier one. None before the walk's first quote.
                            if let Some((mid, width, bid_sz, ask_sz)) = pending_quote {
                                open = Some(OpenParent {
                                    quote_mid: mid,
                                    width_ticks: width,
                                    bid_sz,
                                    ask_sz,
                                    first_ts: t.ts,
                                    first_price: t.px,
                                    first_side: t.side,
                                    rows: 1,
                                    levels: vec![t.px],
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
    flush_minute(&mut acc, &mut minute_state, &scale, tick_f);
    // The two largest minute ranges observed (not distinct values): a
    // repeated maximum is its own second maximum.
    let mut ranges = acc.minute_range_ticks_hist.iter().rev();
    if let Some((&largest, &count)) = ranges.next() {
        acc.minute_range_max_ticks = largest;
        acc.minute_range_second_max_ticks = if count >= 2 {
            largest
        } else {
            ranges.next().map_or(0, |(&next, _)| next)
        };
    }
    let minute_detail: std::collections::BTreeMap<u64, (Decimal, Decimal, u64)> = minute_detail
        .into_iter()
        .map(|(minute, (lo, hi, count))| (minute, (scale.price(lo), scale.price(hi), count)))
        .collect();
    acc.top_minutes = rank_top_minutes(&minute_detail, &minute_parents, tick);
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
    minute_records.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    minute_records.truncate(TOP_MINUTE_RECORDS);
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
