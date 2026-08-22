// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The UNIFIED protocol-12a block engine (the retired rewrite plan, phase
//! 2a). Blocks 1-4, the permutation records and the Block-5 forensics exist
//! here ONCE, and both measurement sides drive them:
//!
//! - the OBSERVED side ([`observed`]) feeds TBBO rows through the phase-1
//!   stream layer, inferring parents from contiguous `(ts, side)` runs;
//! - the GENERATED side ([`generated`]) feeds `mogwai-data` `TickEvent`s and
//!   per-parent `VolTrace` records from a walk.
//!
//! Until this engine lands, the two sides were two independent
//! implementations - `analysis/mnq_fit.py`'s `_M12aSession`/`_m12a_block*`
//! and `crates/mogwai-cli/src/measure12a.rs`'s `Measure12aAcc` - whose
//! agreement was checked only by comparing artifacts. That is the
//! cross-language float-mismatch defect class the rewrite exists to kill.
//! Both originals stay on disk and authoritative until phase 2c: where this
//! port and the Python disagree on the OBSERVED side the Python wins; where
//! it disagrees with `measure12a.rs` on the GENERATED side, `measure12a.rs`
//! wins (its outputs ARE the committed walk caches).
//!
//! ## The arithmetic conventions, and why each is spelled out
//!
//! - **Prices are integers in 1e-9 units everywhere.** The observed corpus
//!   arrives that way; the generated side converts its `Decimal` prices
//!   exactly, refusing anything off the 1e-9 grid. No price is ever rounded
//!   through `f64`.
//! - **The log mid is `ln((bid_nanos + ask_nanos) as f64 / 2.0)`.** The SUM
//!   is formed in exact integers, halved in `f64`, and only then logged. Log
//!   differences are not invariant under rescaling of the input, so feeding
//!   `ln` a mid in points rather than in nano-units shifts every block-3 and
//!   block-4 statistic by a term that does not cancel. A mismatch of that
//!   class (3.55e-15 relative) was caught here once already.
//! - **Tick ranges are exact integer divisions that refuse off-grid.** The
//!   trade range divides by the full tick; the quote-mid range divides the
//!   PRICE SUM by the full tick, which is why it is in HALF ticks.
//! - **Segment iteration follows insertion order**, matching the Python's
//!   `state.order`, because block-3 and block-4 sums are float
//!   accumulations whose value depends on the order the segments contribute.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{LabError, LabResult};
use crate::kernel::{fisher_yates, nearest_rank_list, tuple_mix, weighted_nearest_rank};
use crate::session::{SessionSegment, segment_labels, session_date_int, session_segment_at};
use crate::subcontract::{
    COUNT_WINDOWS_S, PERMUTATION_BASE_SEED, PERMUTATION_REPLICATES, PERMUTATION_VARIANTS,
    RESIDUAL_EXCEED_MULTIPLES, RESIDUAL_MIN_HISTORY, RESIDUAL_WINDOW_S, TICK_UNITS,
    WALL_HORIZONS_S,
};

pub mod forensic;
pub mod generated;
pub mod observed;
mod screen;

pub use screen::{ScreenReduced, ScreenSessionAcc, ScreenWindow};

/// Derived beside the sub-contract in `analysis/mnq_fit.py` (they sit after
/// the `SUBCONTRACT_KEYS` block, so they are not hash inputs).
pub const HORIZON_PAIRS: [(u64, u64); 4] = [(1, 5), (5, 15), (15, 60), (60, 300)];
/// `VARIANT_TAGS`: sign = 0, magnitude = 1 (spec 3.4a).
pub const VARIANT_TAGS: [u64; 2] = [0, 1];
/// `SEGMENT_INDEX`: overnight = 0, post_halt = 1.
pub const SEGMENT_NAMES: [&str; 2] = ["overnight", "post_halt"];

pub(crate) const NS_PER_MIN: u64 = 60_000_000_000;
pub(crate) const NS_PER_HOUR: u64 = 3_600_000_000_000;

/// Which side is measuring. The only thing it changes in the block engine is
/// the `scope` string of the Amendment-F refusal records and whether the
/// spec-5.1 permutation counterfactuals are computed at all (5.1 is
/// observed-side only; the generated side emits an empty array).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Observed,
    Generated { seed: u64 },
}

impl Scope {
    fn session_refusal_scope(self, date: &str) -> String {
        match self {
            Scope::Observed => format!("observed session {date}"),
            Scope::Generated { seed } => format!("seed {seed} session {date}"),
        }
    }

    const fn wants_permutations(self) -> bool {
        matches!(self, Scope::Observed)
    }
}

/// `value / unit` as an exact integer; anything off the grid refuses rather
/// than flooring. The Python floor-divides (`//`) here, which is the same
/// answer for on-grid input and a silent wrong answer otherwise; the port
/// fails closed instead, and the parity gates prove the corpus never hits it.
pub(crate) fn exact_div(value: i64, unit: i64, what: &str) -> LabResult<i64> {
    if unit == 0 || value % unit != 0 {
        return Err(LabError::refusal(format!(
            "off-grid {what}: {value} on unit {unit}"
        )));
    }
    Ok(value / unit)
}

// -- Per-session accumulation state (the unified `_M12aSession`) ------------

#[derive(Debug)]
pub(crate) struct SegmentAcc {
    pub(crate) index: u8,
    pub(crate) origin_ns: u64,
    pub(crate) end_ns: u64,
    /// First-child timestamps of the session's sided parents.
    pub(crate) parent_ts: Vec<u64>,
    /// Endpoint timestamps and log mids of the VALID-BOOK parents only. The
    /// two arrays are index-parallel; `mid_log[i]` is the endpoint at
    /// `mid_ts[i]`, and return `i` is the adjacent difference ENDING at
    /// `mid_ts[i + 1]`.
    pub(crate) mid_ts: Vec<u64>,
    pub(crate) mid_log: Vec<f64>,
}

impl SegmentAcc {
    fn new(index: u8, origin_ns: u64, end_ns: u64) -> Self {
        Self {
            index,
            origin_ns,
            end_ns,
            parent_ts: Vec::new(),
            mid_ts: Vec::new(),
            mid_log: Vec::new(),
        }
    }
}

/// One session's accumulation state, driven identically by both sides.
#[derive(Debug)]
pub struct SessionAcc {
    pub date: String,
    pub session_start_ns: u64,
    pub session_end_ns: u64,
    /// Segment INSERTION order, mirroring the Python `state.order`. Both
    /// calendar segments are materialized at close (an empty segment still
    /// contributes its scheduled zero-count windows and its never-established
    /// horizon chains), so the order is data-dependent only in which of the
    /// two saw its first parent first.
    order: Vec<u8>,
    segments: [Option<SegmentAcc>; 2],
    bounds: [(u64, u64); 2],
    /// minute -> (trade lo, trade hi) in 1e-9 price units, over ALL
    /// structurally valid prints (unsided and invalid-book included).
    trade_min: BTreeMap<u64, (i64, i64)>,
    /// minute -> (lo, hi) quote-mid HALF ticks, valid-book parents only.
    quote_min: BTreeMap<u64, (i64, i64)>,
    /// minute -> sided parent count by first-child timestamp.
    n_min: BTreeMap<u64, u64>,
    count_windows_s: &'static [i64],
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct OrderedCount {
    pub session_date: String,
    pub segment_index: u8,
    pub window_start_ns: u64,
    pub window_end_ns: u64,
    pub endpoint_hour: u64,
    pub parent_count: u32,
}

impl SessionAcc {
    /// Every segment-origin-aligned one-second window, including the windows
    /// that Block 2 later excludes for crossing an hour boundary.
    pub fn ordered_counts(&mut self) -> LabResult<Vec<OrderedCount>> {
        self.seg(0);
        self.seg(1);
        let mut out = Vec::new();
        for seg in self.ordered() {
            let mut i = 0usize;
            for (start, stop, _) in window_schedule(seg.origin_ns, seg.end_ns, 1_000_000_000) {
                while i < seg.parent_ts.len() && seg.parent_ts[i] < start {
                    i += 1;
                }
                let mut j = i;
                while j < seg.parent_ts.len() && seg.parent_ts[j] < stop {
                    j += 1;
                }
                out.push(OrderedCount {
                    session_date: self.date.clone(),
                    segment_index: seg.index,
                    window_start_ns: start,
                    window_end_ns: stop,
                    endpoint_hour: (stop / NS_PER_HOUR) % 24,
                    parent_count: u32::try_from(j - i)
                        .map_err(|_| LabError::refusal("one-second parent count exceeds u32"))?,
                });
                i = j;
            }
        }
        Ok(out)
    }
    /// The two segment windows of one session, derived from the calendar
    /// exactly once. `seg` is any resolution inside the session.
    #[must_use]
    pub fn new(date: String, seg: &SessionSegment, offset_minutes: i32) -> Self {
        Self::new_with_count_windows(date, seg, offset_minutes, COUNT_WINDOWS_S)
    }

    /// Construct the same protocol-12a accumulator with a caller-supplied
    /// Block 2 window set. All windowing and accumulation code remains shared.
    #[must_use]
    pub fn new_with_count_windows(
        date: String,
        seg: &SessionSegment,
        offset_minutes: i32,
        count_windows_s: &'static [i64],
    ) -> Self {
        let overnight = session_segment_at(seg.session_start_ns, offset_minutes)
            .expect("the open instant is inside the overnight segment");
        let post = session_segment_at(seg.session_end_ns - NS_PER_MIN, offset_minutes)
            .expect("the pre-close minute is inside the post-halt segment");
        Self {
            date,
            session_start_ns: seg.session_start_ns,
            session_end_ns: seg.session_end_ns,
            order: Vec::new(),
            segments: [None, None],
            bounds: [
                (overnight.segment_origin_ns, overnight.segment_end_ns),
                (post.segment_origin_ns, post.segment_end_ns),
            ],
            trade_min: BTreeMap::new(),
            quote_min: BTreeMap::new(),
            n_min: BTreeMap::new(),
            count_windows_s,
        }
    }

    /// `_M12aSession.seg`: get-or-create, appending to the insertion order.
    fn seg(&mut self, index: u8) -> &mut SegmentAcc {
        let i = usize::from(index);
        if self.segments[i].is_none() {
            let (origin, end) = self.bounds[i];
            self.segments[i] = Some(SegmentAcc::new(index, origin, end));
            self.order.push(index);
        }
        self.segments[i].as_mut().expect("just inserted")
    }

    fn ordered(&self) -> Vec<&SegmentAcc> {
        self.order
            .iter()
            .map(|i| {
                self.segments[usize::from(*i)]
                    .as_ref()
                    .expect("order only holds live segments")
            })
            .collect()
    }

    /// A structurally valid print at `ts_ns` with price `price_nanos`: widens
    /// the minute's trade range. Every print counts, sided or not, valid book
    /// or not.
    pub fn push_print(&mut self, ts_ns: u64, price_nanos: i64) {
        let entry = self
            .trade_min
            .entry(ts_ns / NS_PER_MIN)
            .or_insert((price_nanos, price_nanos));
        if price_nanos < entry.0 {
            entry.0 = price_nanos;
        }
        if price_nanos > entry.1 {
            entry.1 = price_nanos;
        }
    }

    /// One closed sided parent: its first-child timestamp, the segment it
    /// falls in, and its book. `book_normal` gates the quote-mid endpoint
    /// series - an invalid book contributes a parent count but no mid.
    pub fn push_parent(
        &mut self,
        segment_index: u8,
        first_ts: u64,
        bid_nanos: i64,
        ask_nanos: i64,
        book_normal: bool,
    ) -> LabResult<()> {
        let minute = first_ts / NS_PER_MIN;
        {
            let seg = self.seg(segment_index);
            seg.parent_ts.push(first_ts);
        }
        *self.n_min.entry(minute).or_insert(0) += 1;
        if book_normal {
            let sum = bid_nanos + ask_nanos;
            // The quote-mid range in HALF ticks: the price SUM divided by the
            // full tick. Both prices are on the tick grid, so the sum always
            // divides exactly.
            let mid2 = exact_div(sum, TICK_UNITS, "quote-mid half-ticks")?;
            let entry = self.quote_min.entry(minute).or_insert((mid2, mid2));
            if mid2 < entry.0 {
                entry.0 = mid2;
            }
            if mid2 > entry.1 {
                entry.1 = mid2;
            }
            // THE canonical log-mid: exact integer nano-unit sum, halved in
            // f64, then ln. See the module docs.
            #[expect(
                clippy::cast_precision_loss,
                reason = "a price sum stays far below 2^53"
            )]
            let mid_units = sum as f64 / 2.0;
            let seg = self.seg(segment_index);
            seg.mid_ts.push(first_ts);
            seg.mid_log.push(mid_units.ln());
        }
        Ok(())
    }

    /// `m12a_close_session`: materialize both calendar segments, then run the
    /// blocks. Consumes nothing - the caller owns the lifetime.
    pub fn close(mut self, scope: Scope) -> LabResult<serde_json::Value> {
        self.seg(0);
        self.seg(1);
        let (block3, perm_windows) = self.block3()?;
        let (block4, refusals) = self.block4(scope);
        let segments: Vec<serde_json::Value> = self
            .ordered()
            .iter()
            .map(|seg| {
                serde_json::json!({
                    "segment_index": seg.index,
                    "open_ns": seg.origin_ns,
                    "close_ns": seg.end_ns,
                })
            })
            .collect();
        let permutations = if scope.wants_permutations() {
            self.permutations(&perm_windows)
        } else {
            Vec::new()
        };
        Ok(serde_json::json!({
            "session_date": self.date,
            "segments": segments,
            "block1_hist": self.block1()?,
            "block2": self.block2(),
            "block3": block3,
            "block4": block4,
            "permutations": permutations,
            "refusals": refusals,
        }))
    }

    /// Close only the Stage A projection shared surface. This uses the same
    /// block implementations as `close` and deliberately avoids blocks 3 and
    /// 4 and permutation work.
    ///
    /// A STABLE PROFILE FRAME: once per session rotation, roughly 23 times per
    /// seed-month, so the annotation is free and the frame separates the
    /// per-print accumulation from the per-session reduction - the split the
    /// Stage A round's lean-accumulator hypothesis turns on.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn close_reduced(mut self, _scope: Scope) -> LabResult<serde_json::Value> {
        self.seg(0);
        self.seg(1);
        Ok(serde_json::json!({
            "session_date": self.date,
            "block1_hist": self.block1()?,
            "block2": self.block2(),
        }))
    }

    // -- Block 1 ------------------------------------------------------------

    /// The exact sparse joint histogram (spec 3.5), one record per distinct
    /// key with its occurrence count.
    fn block1(&self) -> LabResult<serde_json::Value> {
        // Keyed for the frozen ascending sort: (n, quote with null FIRST as
        // -1, trade, hour, since, until).
        let mut hist: BTreeMap<(u64, i64, i64, u64, &'static str, &'static str), u64> =
            BTreeMap::new();
        let minutes: BTreeSet<u64> = self
            .trade_min
            .keys()
            .chain(self.n_min.keys())
            .copied()
            .collect();
        for minute in minutes {
            let start_ns = minute * NS_PER_MIN;
            // A structurally valid print outside the session's own open
            // segments is a session-assignment defect (the Python raises
            // Refusal here).
            let seg = self
                .segments
                .iter()
                .flatten()
                .find(|g| start_ns >= g.origin_ns && start_ns < g.end_ns)
                .ok_or_else(|| {
                    LabError::refusal(format!(
                        "minute {minute} carries rows but maps to no open segment of {}",
                        self.date
                    ))
                })?;
            let (since, until) = segment_labels(start_ns, seg.origin_ns, seg.end_ns);
            let hour = (start_ns / NS_PER_HOUR) % 24;
            let trade_ticks = match self.trade_min.get(&minute) {
                Some(&(lo, hi)) => exact_div(hi - lo, TICK_UNITS, "trade range ticks")?,
                None => 0,
            };
            let quote_half = self.quote_min.get(&minute).map_or(-1, |&(lo, hi)| hi - lo);
            let n = self.n_min.get(&minute).copied().unwrap_or(0);
            *hist
                .entry((n, quote_half, trade_ticks, hour, since, until))
                .or_insert(0) += 1;
        }
        Ok(serde_json::Value::Array(
            hist.iter()
                .map(|(&(n, q, t, h, since, until), &count)| {
                    serde_json::json!({
                        "n": n,
                        "quote_range_half_ticks": if q < 0 {
                            serde_json::Value::Null
                        } else {
                            serde_json::json!(q)
                        },
                        "trade_range_ticks": t,
                        "hour": h,
                        "since_open_bin": since,
                        "until_close_bin": until,
                        "count": count,
                    })
                })
                .collect(),
        ))
    }

    // -- Block 2 ------------------------------------------------------------

    fn block2(&self) -> serde_json::Value {
        let mut cells: BTreeMap<(u64, u64), Block2Cell> = BTreeMap::new();
        for seg in self.ordered() {
            for &w in self.count_windows_s {
                #[expect(clippy::cast_sign_loss, reason = "the window lengths are 1, 5 and 60")]
                let w = w as u64;
                let w_ns = w * 1_000_000_000;
                let pts = &seg.parent_ts;
                let mut i = 0usize;
                let mut prev_count: Option<u64> = None;
                let mut prev_hour: Option<u64> = None;
                let mut run = 0u64;
                for (start, stop, hour) in window_schedule(seg.origin_ns, seg.end_ns, w_ns) {
                    while i < pts.len() && pts[i] < start {
                        i += 1;
                    }
                    let mut j = i;
                    while j < pts.len() && pts[j] < stop {
                        j += 1;
                    }
                    let count = (j - i) as u64;
                    i = j;
                    let Some(hour) = hour else {
                        // Hour-crossing - INCLUDING a window ending exactly ON
                        // the hour boundary (endpoint-hour attribution,
                        // matching the fixed-horizon convention): excluded;
                        // runs and pairs reset.
                        close_run(&mut run, prev_hour, w, &mut cells);
                        prev_count = None;
                        prev_hour = None;
                        continue;
                    };
                    if prev_hour.is_some() && prev_hour != Some(hour) {
                        close_run(&mut run, prev_hour, w, &mut cells);
                        prev_count = None;
                    }
                    let cell = cells.entry((hour, w)).or_default();
                    cell.scheduled += 1;
                    if count == 0 {
                        cell.zeros += 1;
                    }
                    *cell.count_hist.entry(count).or_insert(0) += 1;
                    if let Some(prev) = prev_count {
                        cell.paired += 1;
                        cell.sum_x += prev;
                        cell.sum_y += count;
                        cell.sumsq_x += prev * prev;
                        cell.sumsq_y += count * count;
                        cell.sum_xy += prev * count;
                    }
                    prev_count = Some(count);
                    prev_hour = Some(hour);
                    if count > 0 {
                        run += 1;
                    } else {
                        // Closed AFTER prev_hour advanced: an interrupted run
                        // is attributed to the hour of the zero window that
                        // ended it, exactly as the Python closure reads it.
                        close_run(&mut run, prev_hour, w, &mut cells);
                    }
                }
                close_run(&mut run, prev_hour, w, &mut cells);
            }
        }
        let mut out = serde_json::Map::new();
        for (&(hour, w), cell) in &cells {
            out.entry(hour.to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .expect("hour entry is an object")
                .insert(w.to_string(), cell.finish());
        }
        serde_json::Value::Object(out)
    }

    // -- Block 3 ------------------------------------------------------------

    #[expect(
        clippy::type_complexity,
        reason = "the perm-window index is a local shape"
    )]
    fn block3(&self) -> LabResult<(serde_json::Value, PermWindows)> {
        let mut cells: BTreeMap<(u64, u64), ScaleCell> = BTreeMap::new();
        let mut pairs: BTreeMap<(u64, (u64, u64)), (u64, f64, f64)> = BTreeMap::new();
        let mut h20: BTreeMap<(String, u64), ScaleCell> = BTreeMap::new();
        let mut lag1: BTreeMap<u64, [f64; 6]> = BTreeMap::new();
        let mut lag1_n: BTreeMap<u64, u64> = BTreeMap::new();
        let mut perm_windows: PermWindows = BTreeMap::new();

        for seg in self.ordered() {
            // Amendment A: every (segment, hour) the segment SPANS - every UTC
            // hour interval with a positive intersection with [origin, end) -
            // materializes a permutation cell, so hours with no emitted
            // windows still emit all-zero PermRecords.
            let mut t = (seg.origin_ns / NS_PER_HOUR) * NS_PER_HOUR;
            while t < seg.end_ns {
                perm_windows
                    .entry((seg.index, (t / NS_PER_HOUR) % 24))
                    .or_default();
                t += NS_PER_HOUR;
            }
            let series: BTreeMap<u64, Vec<Boundary>> = WALL_HORIZONS_S
                .iter()
                .map(|&h| {
                    #[expect(clippy::cast_sign_loss, reason = "the horizons are 1..300")]
                    let h = h as u64;
                    (h, wall_boundaries(seg, h * 1_000_000_000))
                })
                .collect();
            let asof_at: BTreeMap<u64, BTreeMap<u64, Option<f64>>> = series
                .iter()
                .map(|(&h, rows)| (h, rows.iter().map(|&(b, a, ..)| (b, a)).collect()))
                .collect();
            for h in series.keys().copied() {
                for &(b, _asof, emitted, hour, ret) in &series[&h] {
                    if !emitted {
                        continue;
                    }
                    let r = ret.expect("an emitted boundary carries a return");
                    cells.entry((hour, h)).or_default().push(r);
                    if hour == 20 {
                        let (since, until) = segment_labels(b, seg.origin_ns, seg.end_ns);
                        h20.entry((format!("{since}|{until}"), h))
                            .or_default()
                            .push(r);
                    }
                }
            }
            for &(h, big) in &HORIZON_PAIRS {
                let k = big / h;
                let h_asof = &asof_at[&h];
                let h_ns = h * 1_000_000_000;
                for &(b, _asof, emitted, hour, ret) in &series[&big] {
                    if !emitted {
                        continue;
                    }
                    let big_r = ret.expect("an emitted boundary carries a return");
                    let mut comp2 = 0.0f64;
                    let mut ok = true;
                    for j in 0..k {
                        let hi = h_asof.get(&(b - j * h_ns)).copied().flatten();
                        let lo = h_asof.get(&(b - (j + 1) * h_ns)).copied().flatten();
                        match (hi, lo) {
                            (Some(hi), Some(lo)) => {
                                let d = hi - lo;
                                comp2 += d * d;
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        continue;
                    }
                    let entry = pairs.entry((hour, (h, big))).or_insert((0, 0.0, 0.0));
                    entry.0 += 1;
                    entry.1 += big_r * big_r;
                    entry.2 += comp2;
                }
            }
            // Permutation window index ranges: for 60/300, emitted windows map
            // to the half-open RETURN index range whose endpoint ts lies in
            // (b - W, b]. Return i is the adjacent-mid return ending at
            // mid_ts[i + 1], so the endpoint index is one ahead.
            let ts = &seg.mid_ts;
            let n_mid = ts.len();
            for h in [60u64, 300u64] {
                let w_ns = h * 1_000_000_000;
                let mut i = 1usize;
                for &(b, _asof, emitted, hour, _ret) in &series[&h] {
                    let lo_ts = b - w_ns;
                    while i < n_mid && ts[i] <= lo_ts {
                        i += 1;
                    }
                    let mut j = i;
                    while j < n_mid && ts[j] <= b {
                        j += 1;
                    }
                    if emitted {
                        perm_windows
                            .entry((seg.index, hour))
                            .or_default()
                            .entry(h)
                            .or_default()
                            .push((i - 1, j - 1));
                    }
                    i = j;
                }
            }
            // Lag-1 parent-return scalar: consecutive adjacent-mid returns in
            // the segment, attributed by the LATER return's endpoint hour.
            let mut prev_r: Option<f64> = None;
            for idx in 1..seg.mid_ts.len() {
                let r = seg.mid_log[idx] - seg.mid_log[idx - 1];
                if let Some(p) = prev_r {
                    let hour = (seg.mid_ts[idx] / NS_PER_HOUR) % 24;
                    let acc = lag1.entry(hour).or_insert([0.0; 6]);
                    *lag1_n.entry(hour).or_insert(0) += 1;
                    acc[0] += p;
                    acc[1] += r;
                    acc[2] += p * p;
                    acc[3] += r * r;
                    acc[4] += p * r;
                }
                prev_r = Some(r);
            }
        }

        let mut cells_out = serde_json::Map::new();
        for (&(hour, h), cell) in &cells {
            cells_out
                .entry(hour.to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .expect("hour entry is an object")
                .insert(h.to_string(), cell.json());
        }
        let mut pairs_out = serde_json::Map::new();
        for (&(hour, (h, big)), &(n, sum_rh2, sum_comp2)) in &pairs {
            #[expect(clippy::cast_precision_loss, reason = "window counts stay small")]
            let nf = n as f64;
            pairs_out
                .entry(hour.to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .expect("hour entry is an object")
                .insert(
                    format!("{h}-{big}"),
                    serde_json::json!({
                        "window_count": n,
                        "vr": if sum_comp2 > 0.0 {
                            serde_json::json!(sum_rh2 / sum_comp2)
                        } else {
                            serde_json::Value::Null
                        },
                        "cov_contrib": if n > 0 {
                            serde_json::json!((sum_rh2 - sum_comp2) / nf)
                        } else {
                            serde_json::Value::Null
                        },
                        "cov_contrib_norm": if sum_rh2 > 0.0 {
                            serde_json::json!((sum_rh2 - sum_comp2) / sum_rh2)
                        } else {
                            serde_json::Value::Null
                        },
                    }),
                );
        }
        let mut h20_out = serde_json::Map::new();
        for ((labels, h), cell) in &h20 {
            h20_out
                .entry(labels.clone())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .expect("label entry is an object")
                .insert(h.to_string(), cell.json());
        }
        let lag1_out: serde_json::Map<String, serde_json::Value> = lag1
            .iter()
            .map(|(&hour, acc)| {
                let n = lag1_n.get(&hour).copied().unwrap_or(0);
                (
                    hour.to_string(),
                    match pearson(n, acc) {
                        Some(v) => serde_json::json!(v),
                        None => serde_json::Value::Null,
                    },
                )
            })
            .collect();
        Ok((
            serde_json::json!({
                "cells": cells_out,
                "pairs": pairs_out,
                "lag1_parent_autocorr": lag1_out,
                "hour20_labels": h20_out,
            }),
            perm_windows,
        ))
    }

    // -- Block 4 ------------------------------------------------------------

    /// The model-free past-only standardizer: trailing 300 s same-segment
    /// history, minimum 1000 returns, one-max-trimmed mean absolute scale
    /// EXCLUDING the current return, `z = r / scale`. Zeros stay in history
    /// and in the population. A nonpositive or non-finite scale OMITS that
    /// residual (the return still enters history) per the rev-11 frozen
    /// exception; the omissions come back as one Amendment-F RefusalRec per
    /// (session, hour) - the sole RefusalRec class owning OMITTED
    /// observations rather than refusal-caused nulls. No duplicate record for
    /// the pooled "all" cell.
    fn block4(&self, scope: Scope) -> (serde_json::Value, Vec<serde_json::Value>) {
        let mut per_hour: BTreeMap<String, HourAcc> = BTreeMap::new();
        let mut omitted: BTreeMap<u64, u64> = BTreeMap::new();
        let win_ns = RESIDUAL_WINDOW_S as u64 * 1_000_000_000;
        let min_history = RESIDUAL_MIN_HISTORY as usize;
        for seg in self.ordered() {
            let ts = &seg.mid_ts;
            let logmid = &seg.mid_log;
            let mut window: std::collections::VecDeque<(u64, f64)> =
                std::collections::VecDeque::new();
            let mut maxq: std::collections::VecDeque<f64> = std::collections::VecDeque::new();
            let mut run_sum = 0.0f64;
            for idx in 1..ts.len() {
                let t = ts[idx];
                let r = logmid[idx] - logmid[idx - 1];
                let hour = (t / NS_PER_HOUR) % 24;
                let lo = t.saturating_sub(win_ns);
                while let Some(&(old_ts, old_abs)) = window.front() {
                    if old_ts >= lo {
                        break;
                    }
                    window.pop_front();
                    run_sum -= old_abs;
                    if maxq.front().copied() == Some(old_abs) {
                        maxq.pop_front();
                    }
                }
                let count = window.len();
                if count < min_history {
                    per_hour
                        .entry(hour.to_string())
                        .or_default()
                        .burn_in_excluded += 1;
                    per_hour
                        .entry("all".to_string())
                        .or_default()
                        .burn_in_excluded += 1;
                } else {
                    let mx = maxq.front().copied().unwrap_or(0.0);
                    #[expect(clippy::cast_precision_loss, reason = "count is at least 1000")]
                    let scale = (run_sum - mx) / (count - 1) as f64;
                    if !scale.is_finite() || scale <= 0.0 {
                        *omitted.entry(hour).or_insert(0) += 1;
                    } else {
                        let az = (r / scale).abs();
                        for key in [hour.to_string(), "all".to_string()] {
                            let acc = per_hour.entry(key).or_default();
                            acc.residual_count += 1;
                            if r == 0.0 {
                                acc.zeros += 1;
                            } else {
                                acc.nz_abs.push(az);
                            }
                            for (slot, &bound) in
                                acc.exceed.iter_mut().zip(RESIDUAL_EXCEED_MULTIPLES)
                            {
                                #[expect(clippy::cast_precision_loss, reason = "4, 8, 16")]
                                let bound = bound as f64;
                                if az > bound {
                                    *slot += 1;
                                }
                            }
                        }
                    }
                }
                let a = r.abs();
                window.push_back((t, a));
                run_sum += a;
                while maxq.back().is_some_and(|&b| b < a) {
                    maxq.pop_back();
                }
                maxq.push_back(a);
            }
        }
        // The Python sorts `per_hour` by `str(key)`, i.e. the numeric hours in
        // lexicographic order ("0", "1", "10", ...) and "all" LAST - which is
        // exactly BTreeMap<String> order.
        let mut out = serde_json::Map::new();
        for (key, acc) in &mut per_hour {
            acc.nz_abs.sort_by(f64::total_cmp);
            let rc = acc.residual_count;
            #[expect(
                clippy::cast_precision_loss,
                reason = "residual counts stay far below 2^53"
            )]
            let rcf = rc as f64;
            let p90 = nearest_rank_list(&acc.nz_abs, 0.90);
            let p99 = nearest_rank_list(&acc.nz_abs, 0.99);
            let p999 = nearest_rank_list(&acc.nz_abs, 0.999);
            // The ratio nulls follow the Python truthiness convention: a zero
            // or missing component gives null, never a division.
            let ratio = |a: Option<f64>, b: Option<f64>| match (a, b) {
                (Some(x), Some(y)) if x != 0.0 && y != 0.0 => serde_json::json!(x / y),
                _ => serde_json::Value::Null,
            };
            let frac = |num: u64| {
                if rc > 0 {
                    #[expect(clippy::cast_precision_loss, reason = "see above")]
                    let v = num as f64 / rcf;
                    serde_json::json!(v)
                } else {
                    serde_json::Value::Null
                }
            };
            out.insert(
                key.clone(),
                serde_json::json!({
                    "residual_count": rc,
                    // Frozen artifact spelling of the burn-in exclusion count.
                    "warmup_excluded": acc.burn_in_excluded,
                    "zero_fraction": frac(acc.zeros),
                    "nz_abs_p90": p90,
                    "nz_abs_p99": p99,
                    "nz_abs_p999": p999,
                    "ratio_p99_p90": ratio(p99, p90),
                    "ratio_p999_p99": ratio(p999, p99),
                    "exceed_4": frac(acc.exceed[0]),
                    "exceed_8": frac(acc.exceed[1]),
                    "exceed_16": frac(acc.exceed[2]),
                }),
            );
        }
        let refusals = omitted
            .iter()
            .map(|(&hour, &n)| {
                serde_json::json!({
                    "scope": scope.session_refusal_scope(&self.date),
                    "cell": format!("block4 hour {hour} standardizer"),
                    "reason": format!(
                        "{n} residuals omitted: nonpositive or non-finite trailing scale"
                    ),
                })
            })
            .collect();
        (serde_json::Value::Object(out), refusals)
    }

    // -- Spec 5.1 permutations (observed side only) --------------------------

    /// The 5.1 counterfactuals: per (segment, hour) cell, both variants, 16
    /// replicates. The boundary partition and establishment depend only on
    /// timestamps, so each emitted window's permuted return is the sum of
    /// permuted cell values over its FIXED index range.
    fn permutations(&self, perm_windows: &PermWindows) -> Vec<serde_json::Value> {
        let mut records = Vec::new();
        let date_int = session_date_int(&self.date);
        #[expect(clippy::cast_sign_loss, reason = "session dates are positive YYYYMMDD")]
        let date_int = date_int as u64;
        for seg in self.ordered() {
            let logmid = &seg.mid_log;
            let returns: Vec<f64> = (1..logmid.len())
                .map(|i| logmid[i] - logmid[i - 1])
                .collect();
            let ts = &seg.mid_ts;
            let mut by_hour: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
            for i in 0..returns.len() {
                by_hour
                    .entry((ts[i + 1] / NS_PER_HOUR) % 24)
                    .or_default()
                    .push(i);
            }
            let hours_here: Vec<u64> = perm_windows
                .keys()
                .filter(|&&(s, _)| s == seg.index)
                .map(|&(_, h)| h)
                .collect();
            for hour in hours_here {
                let empty = BTreeMap::new();
                let pw = perm_windows.get(&(seg.index, hour)).unwrap_or(&empty);
                // The cell's return positions: endpoint hour == hour. The
                // emitted windows lie wholly inside the hour, so every index
                // they cover belongs to the cell. A cell with ZERO adjacent
                // returns still emits its records (Amendment A).
                let cell_idx: &[usize] = by_hour.get(&hour).map_or(&[], Vec::as_slice);
                let values: Vec<f64> = cell_idx.iter().map(|&i| returns[i]).collect();
                let pos_of: BTreeMap<usize, usize> = cell_idx
                    .iter()
                    .enumerate()
                    .map(|(k, &orig)| (orig, k))
                    .collect();
                let windows: BTreeMap<u64, Vec<Vec<usize>>> = [60u64, 300u64]
                    .into_iter()
                    .map(|h| {
                        let ranges: &[(usize, usize)] = pw.get(&h).map_or(&[], Vec::as_slice);
                        let built = ranges
                            .iter()
                            .map(|&(lo, hi)| {
                                (lo..hi).filter_map(|i| pos_of.get(&i).copied()).collect()
                            })
                            .collect();
                        (h, built)
                    })
                    .collect();
                let nz_pos: Vec<usize> = values
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| **v != 0.0)
                    .map(|(k, _)| k)
                    .collect();
                for (vi, variant) in PERMUTATION_VARIANTS.iter().enumerate() {
                    let vtag = VARIANT_TAGS[vi];
                    for rep in 0..PERMUTATION_REPLICATES {
                        #[expect(clippy::cast_sign_loss, reason = "frozen positive constants")]
                        let state_seed = tuple_mix(
                            PERMUTATION_BASE_SEED as u64,
                            &[date_int, u64::from(seg.index), hour, vtag, rep as u64],
                        );
                        let mut perm = nz_pos.clone();
                        fisher_yates(&mut perm, state_seed);
                        let mut shuffled = values.clone();
                        if *variant == "sign" {
                            // Shuffle SIGNS among the nonzero: magnitude order
                            // fixed, sign sequence permuted.
                            let signs: Vec<f64> = perm
                                .iter()
                                .map(|&p| if values[p] > 0.0 { 1.0 } else { -1.0 })
                                .collect();
                            for (k, &p) in nz_pos.iter().enumerate() {
                                shuffled[p] = values[p].abs() * signs[k];
                            }
                        } else {
                            // Shuffle MAGNITUDES among the nonzero: sign
                            // sequence fixed, magnitudes permuted.
                            let mags: Vec<f64> = perm.iter().map(|&p| values[p].abs()).collect();
                            for (k, &p) in nz_pos.iter().enumerate() {
                                let sgn = if values[p] > 0.0 { 1.0 } else { -1.0 };
                                shuffled[p] = sgn * mags[k];
                            }
                        }
                        let mut rec = serde_json::Map::new();
                        rec.insert("segment_index".into(), serde_json::json!(seg.index));
                        rec.insert("hour".into(), serde_json::json!(hour));
                        rec.insert("variant".into(), serde_json::json!(variant));
                        rec.insert("replicate".into(), serde_json::json!(rep));
                        for h in [60u64, 300u64] {
                            let (cnt, s_abs, m_abs) = windows_stats(&shuffled, &windows[&h]);
                            rec.insert(format!("return_count_{h}"), serde_json::json!(cnt));
                            rec.insert(format!("sum_abs_{h}"), serde_json::json!(s_abs));
                            rec.insert(format!("max_abs_{h}"), serde_json::json!(m_abs));
                        }
                        records.push(serde_json::Value::Object(rec));
                    }
                }
            }
        }
        records
    }
}

/// Permutation index ranges: `(segment index, hour) -> horizon -> [(lo, hi)]`
/// half-open RETURN index ranges of the emitted windows.
type PermWindows = BTreeMap<(u8, u64), BTreeMap<u64, Vec<(usize, usize)>>>;

/// `(count, sum_abs, max_abs)` over the emitted windows' return sums - the
/// Amendment-A sufficient statistics. Emitted zero returns COUNT; an all-zero
/// population has zero sums.
fn windows_stats(values: &[f64], windows: &[Vec<usize>]) -> (u64, f64, f64) {
    let mut cnt = 0u64;
    let mut s_abs = 0.0f64;
    let mut m_abs = 0.0f64;
    for widx in windows {
        let mut s = 0.0f64;
        for &p in widx {
            s += values[p];
        }
        let a = s.abs();
        cnt += 1;
        s_abs += a;
        if a > m_abs {
            m_abs = a;
        }
    }
    (cnt, s_abs, m_abs)
}

// -- Block 2 cell -----------------------------------------------------------

#[derive(Default, Debug)]
struct Block2Cell {
    scheduled: u64,
    zeros: u64,
    count_hist: BTreeMap<u64, u64>,
    run_hist: BTreeMap<u64, u64>,
    paired: u64,
    sum_x: u64,
    sum_y: u64,
    sumsq_x: u64,
    sumsq_y: u64,
    sum_xy: u64,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "every count stays far below 2^53"
)]
impl Block2Cell {
    fn finish(&self) -> serde_json::Value {
        let total: u64 = self.count_hist.values().sum();
        let ssum: u64 = self.count_hist.iter().map(|(&k, &v)| k * v).sum();
        let ssq: u64 = self.count_hist.iter().map(|(&k, &v)| k * k * v).sum();
        let mean = if total > 0 {
            Some(ssum as f64 / total as f64)
        } else {
            None
        };
        let var = mean.map(|m| ssq as f64 / total as f64 - m * m);
        let fano = match (mean, var) {
            (Some(m), Some(v)) if m != 0.0 => Some(v / m),
            _ => None,
        };
        let n = self.paired;
        let lag1 = if n >= 2 {
            let nf = n as f64;
            let vx = self.sumsq_x as f64 - (self.sum_x as f64).powi(2) / nf;
            let vy = self.sumsq_y as f64 - (self.sum_y as f64).powi(2) / nf;
            if vx > 0.0 && vy > 0.0 {
                Some(
                    (self.sum_xy as f64 - self.sum_x as f64 * self.sum_y as f64 / nf)
                        / (vx * vy).sqrt(),
                )
            } else {
                None
            }
        } else {
            None
        };
        let count_pairs: Vec<(i64, i64)> = self
            .count_hist
            .iter()
            .map(|(&k, &v)| (k as i64, v as i64))
            .collect();
        let run_pairs: Vec<(i64, i64)> = self
            .run_hist
            .iter()
            .map(|(&k, &v)| (k as i64, v as i64))
            .collect();
        serde_json::json!({
            "scheduled_windows": self.scheduled,
            "zero_windows": self.zeros,
            "count_hist": self.count_hist.iter()
                .map(|(&k, &v)| (k.to_string(), serde_json::json!(v)))
                .collect::<serde_json::Map<_, _>>(),
            "run_length_hist": self.run_hist.iter()
                .map(|(&k, &v)| (k.to_string(), serde_json::json!(v)))
                .collect::<serde_json::Map<_, _>>(),
            "paired_lag_count": n,
            "sum_x": self.sum_x, "sum_y": self.sum_y,
            "sumsq_x": self.sumsq_x, "sumsq_y": self.sumsq_y,
            "sum_xy": self.sum_xy,
            "zero_fraction": if self.scheduled > 0 {
                serde_json::json!(self.zeros as f64 / self.scheduled as f64)
            } else {
                serde_json::Value::Null
            },
            "mean": mean,
            "fano": fano,
            "count_p90": weighted_nearest_rank(&count_pairs, 0.90),
            "count_p99": weighted_nearest_rank(&count_pairs, 0.99),
            "count_p999": weighted_nearest_rank(&count_pairs, 0.999),
            "lag1_autocorr": lag1,
            "run_p90": if run_pairs.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::json!(weighted_nearest_rank(&run_pairs, 0.90))
            },
        })
    }
}

fn close_run(
    run: &mut u64,
    prev_hour: Option<u64>,
    w: u64,
    cells: &mut BTreeMap<(u64, u64), Block2Cell>,
) {
    if *run > 0
        && let Some(h) = prev_hour
    {
        *cells
            .entry((h, w))
            .or_default()
            .run_hist
            .entry(*run)
            .or_insert(0) += 1;
    }
    *run = 0;
}

/// The pure-calendar window schedule ONE iterator both the block-2
/// accumulator and (in phase 2b) the expected-exposure judge consume, so
/// their endpoint rules cannot drift: `(start, stop, endpoint_hour | None)`
/// for every segment-origin-aligned half-open window STRICTLY CONTAINED in
/// the segment. `None` marks an hour-crossing window - endpoint-hour
/// attribution, so a window ending exactly ON the hour boundary crosses.
pub fn window_schedule(origin_ns: u64, end_ns: u64, w_ns: u64) -> Vec<(u64, u64, Option<u64>)> {
    let mut out = Vec::new();
    let mut start = origin_ns;
    while start + w_ns <= end_ns {
        let stop = start + w_ns;
        let s_hour = (start / NS_PER_HOUR) % 24;
        let e_hour = (stop / NS_PER_HOUR) % 24;
        out.push((
            start,
            stop,
            if s_hour == e_hour { Some(e_hour) } else { None },
        ));
        start = stop;
    }
    out
}

// -- Block 3 helpers --------------------------------------------------------

/// One wall boundary: `(boundary, asof, emitted, hour, ret)`.
type Boundary = (u64, Option<f64>, bool, u64, Option<f64>);

/// The 4.6-convention fixed-horizon chain over one segment's valid-mid
/// endpoint series. `asof` at a boundary is the log mid of the last endpoint
/// with `ts <= boundary` (EQUAL timestamps update first - the protocol-11
/// pending rule). The first boundary with an `asof` ESTABLISHES the chain; a
/// boundary emits unless its window crosses a UTC hour; boundaries run
/// `origin + k*W` strictly inside the segment.
fn wall_boundaries(seg: &SegmentAcc, h_ns: u64) -> Vec<Boundary> {
    let ts = &seg.mid_ts;
    let logmid = &seg.mid_log;
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut prev: Option<f64> = None;
    let mut b = seg.origin_ns + h_ns;
    while b < seg.end_ns {
        while i < ts.len() && ts[i] <= b {
            i += 1;
        }
        let asof = if i > 0 { Some(logmid[i - 1]) } else { None };
        let mut emitted = false;
        let mut ret = None;
        if let Some(cur) = asof {
            if let Some(p) = prev {
                let b_hour = (b / NS_PER_HOUR) % 24;
                let s_hour = ((b - h_ns) / NS_PER_HOUR) % 24;
                if b_hour == s_hour {
                    emitted = true;
                    ret = Some(cur - p);
                }
            }
            prev = Some(cur);
        }
        out.push((b, asof, emitted, (b / NS_PER_HOUR) % 24, ret));
        b += h_ns;
    }
    out
}

#[derive(Default, Clone, Copy, Debug)]
struct ScaleCell {
    n: u64,
    sum: f64,
    sumsq: f64,
    sum_abs: f64,
    max_abs: f64,
}

impl ScaleCell {
    fn push(&mut self, r: f64) {
        self.n += 1;
        self.sum += r;
        self.sumsq += r * r;
        let a = r.abs();
        self.sum_abs += a;
        if a > self.max_abs {
            self.max_abs = a;
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "return counts stay far below 2^53"
    )]
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "return_count": self.n,
            "robust_scale": if self.n >= 2 {
                serde_json::json!((self.sum_abs - self.max_abs) / (self.n - 1) as f64)
            } else {
                serde_json::Value::Null
            },
            "rms_scale": if self.n > 0 {
                serde_json::json!((self.sumsq / self.n as f64).sqrt())
            } else {
                serde_json::Value::Null
            },
        })
    }
}

/// The Python `corr`: Pearson over the lag-1 sufficient moments
/// `[sx, sy, sxx, syy, sxy]`, refusing a nonpositive variance.
#[expect(
    clippy::cast_precision_loss,
    reason = "pair counts stay far below 2^53"
)]
fn pearson(n: u64, acc: &[f64; 6]) -> Option<f64> {
    if n < 2 {
        return None;
    }
    let nf = n as f64;
    let (sx, sy, sxx, syy, sxy) = (acc[0], acc[1], acc[2], acc[3], acc[4]);
    let vx = sxx - sx * sx / nf;
    let vy = syy - sy * sy / nf;
    if vx <= 0.0 || vy <= 0.0 {
        return None;
    }
    Some((sxy - sx * sy / nf) / (vx * vy).sqrt())
}

#[derive(Default, Debug)]
struct HourAcc {
    residual_count: u64,
    burn_in_excluded: u64,
    zeros: u64,
    nz_abs: Vec<f64>,
    exceed: [u64; 3],
}

#[cfg(test)]
mod tests;
