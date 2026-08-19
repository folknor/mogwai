// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The `gen --type summary` accumulator: the MNQ TBBO fit's calibration
//! instrument, MOVED here from `mogwai-cli`'s `gen.rs` at phase 3b so the
//! protocol-11 fit driver can run its walks IN-PROCESS instead of shelling
//! the `gen --type summary` subcommand the way `mnq_fit.py` did.
//!
//! `gen.rs` keeps its CLI surface unchanged and calls straight into
//! `summarize` here; `--type summary` output is byte-identical, which the
//! phase's move gate pins with a before/after comparison.
//!
//! This is the one module in `mogwai-lab` that depends on `mogwai-server`:
//! an `InstrumentProfile` is the walk's instrument, and the fit resolves one
//! through the server's own `Config::load` path exactly as the Python's
//! scratch-config walks did.

use mogwai_data::{TickEvent, TickSource};
use mogwai_protocol::AggressorSide;
use rust_decimal::Decimal;

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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionSegment {
    pub session_start_ns: u64,
    pub session_end_ns: u64,
    pub segment_origin_ns: u64,
    pub segment_end_ns: u64,
}

/// Maps an OPEN instant to its session segment per spec 4.5: a local minute
/// at or past 17:00 opens the session dated the next civil day; one before
/// 15:15 belongs to the overnight segment of the prior day's open; the span
/// from 15:30 to 16:00 is the post-halt segment. Closed instants (halt,
/// break) return None - the calendar keeps generated events out of them, so
/// a None on a real event is a session-math defect, not a data case.
///
/// ONE IMPLEMENTATION, IN [`crate::session`]. This was a second copy of the
/// branch structure and of the four session-minute constants, kept honest by
/// `mogwai-cli`'s `session_segment_at_agrees_with_mogwai_lab` - a gate whose
/// own comment called it a temporary bridge until `mogwai-cli` was rewired
/// onto the lab. That rewire has since landed (this module IS what `gen.rs`
/// calls), leaving a permanent two-copy gate with no external anchor: if the
/// session definition moved, both copies moved together and the gate stayed
/// green. The copy is gone rather than the gate strengthened, because one
/// implementation is what a two-copy gate is trying to approximate.
///
/// The two structs are not the same type and that is deliberate: the
/// [`crate::session`] one also reports the trade-date day number and the
/// segment name, which every caller here would have to ignore. The mapping
/// below is the whole difference.
pub fn session_segment_at(ts: u64, offset_minutes: i16) -> Option<SessionSegment> {
    crate::session::session_segment_at(ts, i32::from(offset_minutes)).map(|seg| SessionSegment {
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
    // Per-minute trade-price ranges in INTEGER TICKS over minutes carrying
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
    // the protocol-12 spec consumes. Bounded at TOP_MINUTE_RECORDS.
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
pub fn summarize(
    source: &mut dyn TickSource,
    profile: &mogwai_server::source::InstrumentProfile,
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
    // (horizon_ns, next_k, prev_boundary_mid), plus the SEGMENT-LOCAL
    // as-of mid. Boundaries are segment_origin + k * horizon, k >= 1,
    // strictly inside the segment (spec 4.6); the first boundary having
    // an as-of mid establishes. The as-of is deliberately NOT the global
    // last_mid: state is independent per segment (rule 1), so a pre-halt
    // quote must never establish or price a post-halt boundary - the
    // as-of resets to None on every segment change, exactly as the
    // observed harness resets its chain.
    let mut seg_state: Option<(SessionSegment, HorizonState, Option<f64>)> = None;
    // Worst-minute detail: minute index -> (low, high, trade_count) on the
    // exact price grid, plus parents by first-child minute (spec 4.7).
    let mut minute_detail: std::collections::BTreeMap<u64, (Decimal, Decimal, u64)> =
        Default::default();
    let mut minute_parents: std::collections::BTreeMap<u64, u64> = Default::default();

    // Advance the active segment's boundaries up to `until_ts` - exclusive
    // during the walk (a boundary equal to an event timestamp waits for a
    // later event, so equal-timestamp quotes update the as-of mid first -
    // spec 4.6 rule 6), inclusive at a flush. `as_of` is the last valid
    // quote mid seen so far; `window` is (measured_from, measured_until).
    fn advance_segment_boundaries(
        sessions: &mut std::collections::BTreeMap<u64, GeneratedSessionCells>,
        seg: &SessionSegment,
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
                    // Rule 4: the first boundary HAVING an as-of mid
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
    // (minute index, low, high) of the in-window minute being accumulated.
    let mut minute_state: Option<(u64, f64, f64)> = None;

    fn flush_minute(acc: &mut SummaryAcc, state: &mut Option<(u64, f64, f64)>, tick_f: f64) {
        if let Some((_minute, lo, hi)) = state.take() {
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
                    finalize(
                        &mut acc,
                        &mut prev_parent,
                        &mut sessions,
                        &mut prev_vol_parent,
                        &mut minute_parents,
                        parent,
                    );
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
                // Segment-local as-of: only a quote INSIDE the active
                // segment feeds it - never one from closed time or another
                // segment (rule 1's independence).
                if let Some(offset) = cal_offset
                    && let Some((current, _, seg_mid)) = &mut seg_state
                    && session_segment_at(ts, offset).as_ref() == Some(current)
                {
                    *seg_mid = Some(mid);
                }
            }
            TickEvent::Trade(t) => {
                if t.ts_event >= start && t.ts_event < end {
                    *acc.size_histogram.entry(decimal_key(t.size)).or_insert(0) += 1;
                    let minute = t.ts_event / 60_000_000_000;
                    let price = f64::try_from(t.price).unwrap_or(f64::NAN);
                    match &mut minute_state {
                        Some((current, lo, hi)) if *current == minute => {
                            *lo = lo.min(price);
                            *hi = hi.max(price);
                        }
                        _ => {
                            flush_minute(&mut acc, &mut minute_state, tick_f);
                            minute_state = Some((minute, price, price));
                        }
                    }
                    // Protocol-11 worst-minute detail on the EXACT grid: the
                    // records serialize decimal prices and an exact integer
                    // tick range, where the legacy histogram rounds in f64.
                    minute_detail
                        .entry(minute)
                        .and_modify(|(lo, hi, count)| {
                            if t.price < *lo {
                                *lo = t.price;
                            }
                            if t.price > *hi {
                                *hi = t.price;
                            }
                            *count += 1;
                        })
                        .or_insert((t.price, t.price, 1));
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
        finalize(
            &mut acc,
            &mut prev_parent,
            &mut sessions,
            &mut prev_vol_parent,
            &mut minute_parents,
            parent,
        );
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
    flush_minute(&mut acc, &mut minute_state, tick_f);
    // The two largest minute ranges OBSERVED (not distinct values): a
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
