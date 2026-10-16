// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The generated front-end of the unified block engine: the walk consumer a
//! `mogwai-data` `GeneratedSource` drives. Ported from
//! `crates/mogwai-cli/src/measure12a.rs`'s `Measure12aAcc`, which stays on
//! disk and authoritative for this side until phase 2c - its outputs are the
//! committed walk caches.
//!
//! The consumer only reads events and `VolTrace` records: it neither adds nor
//! changes any field, branch, callback, buffer or draw in `GeneratedSource`,
//! so the 12a landing owed no `TAPE_PROTOCOL_VERSION` bump (spec 2.3; the
//! constant has since moved for unrelated generator changes).
//!
//! Parents come from the source, not from inference: [`GeneratedAcc::walk`]
//! and [`GeneratedAcc::push_parent`] read one whole parent at a time from a
//! `ParentSource`, because quotes are not a parent boundary signal - the
//! discrete-book path can publish two quotes for one parent or none. Within a
//! parent the first sided trade opens it against the book in force (the last
//! quote at or before that trade, in this parent or an earlier one) and the
//! parent's own `VolTrace`; an unsided print terminates the open parent,
//! exactly as an `N` row does on the observed side. No quantity is read, so
//! the walk takes its parents unsized.
//!
//! [`GeneratedAcc::push_quote`] and [`GeneratedAcc::push_trade`] remain as
//! the event-shaped entry for hand-built streams with no parent divisions:
//! there a quote closes the open parent and stands in for the boundary.
//!
//! The session half - which prints and parents are measured, session
//! rotation, complete-session emission - is [`SessionFrame`], shared with the
//! protocol 12b control's accumulator, which wants blocks 1 to 3 and nothing
//! else. The Block-5 forensics ride on the frame as a [`FrameObserver`], so
//! every parent the frame closes reaches them at the instant it closes,
//! including the close a session rotation forces from inside a print.

use std::collections::BTreeMap;

use mogwai_data::{ParentSource, RecordScale, UnsizedParentView, UnsizedRecord, VolTrace};
use mogwai_protocol::{AggressorSide, QuoteTick, TradeTick};
use rust_decimal::Decimal;

use crate::error::{LabError, LabResult};
use crate::measure12a::forensic::{MinuteRec, innovation_bounds, select};
use crate::measure12a::{NS_PER_MIN, Scope, SessionAcc, exact_div};
use crate::session::{SegmentCursor, SessionSegment};
use crate::subcontract::TICK_UNITS;

/// Local copy of the `mogwai-data` ARCH coefficient. `mogwai-data` is
/// untouchable in 12a (spec 2.3), so the value is duplicated here; the
/// `mogwai-cli` twin's `arch_coefficients_match_the_shipped_recursion` pins
/// both against the shipped recursion by recovering them from consecutive
/// `VolTrace` records of a real walk.
pub const ARCH_12A: f64 = 0.02;
/// The GARCH coefficient, carried for the same pinning test.
pub const GARCH_12A: f64 = 0.979;

/// Price in exact 1e-9 units; anything off that grid refuses rather than
/// rounding through `f64`.
fn price_nanos(value: Decimal) -> LabResult<i64> {
    let scaled = value * Decimal::new(1_000_000_000, 0);
    if scaled.fract() != Decimal::ZERO {
        return Err(LabError::refusal(format!(
            "price {value} is off the 1e-9 grid"
        )));
    }
    i64::try_from(scaled.trunc())
        .map_err(|_| LabError::refusal(format!("price {value} overflows i64 nano units")))
}

/// A record price mantissa at `dp` places in exact 1e-9 units, by integer
/// arithmetic alone. A scale coarser than nine places multiplies, checked; a
/// finer one divides only when the mantissa lies on the 1e-9 grid, and
/// refuses otherwise rather than truncating - the same two refusals
/// [`price_nanos`] makes for a decimal.
pub(crate) fn mantissa_nanos(mantissa: i64, dp: u32) -> LabResult<i64> {
    if dp <= 9 {
        let up = 10_i64.pow(9 - dp);
        mantissa.checked_mul(up).ok_or_else(|| {
            LabError::refusal(format!(
                "price mantissa {mantissa} at {dp} places overflows i64 nano units"
            ))
        })
    } else {
        // `dp` is at most `MAX_RECORD_DP` (18), so `10^(dp - 9)` fits.
        let down = 10_i64.checked_pow(dp - 9).ok_or_else(|| {
            LabError::refusal(format!("a {dp}-place price scale is past the record limit"))
        })?;
        if mantissa % down != 0 {
            return Err(LabError::refusal(format!(
                "price mantissa {mantissa} at {dp} places is off the 1e-9 grid"
            )));
        }
        Ok(mantissa / down)
    }
}

/// The tick in nano units and as a float, as both accumulators derive it.
pub(crate) fn tick_units(tick: Decimal) -> (i64, f64) {
    let tick_nanos = price_nanos(tick).unwrap_or(TICK_UNITS);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a tick is a small exact decimal"
    )]
    let tick_f = tick_nanos as f64 / 1e9;
    (tick_nanos, tick_f)
}

struct OpenParent {
    first_ts: u64,
    bid_nanos: i64,
    ask_nanos: i64,
    normal_book: bool,
    trace: Option<VolTrace>,
}

/// A measured parent as the frame closes it: first child inside the window
/// and inside a segment of the lab's session frame.
pub struct ClosedParent {
    pub first_ts: u64,
    pub minute: u64,
    /// 0 overnight, 1 post-halt.
    pub segment_index: u8,
    /// The quote mid in half ticks; `None` for an abnormal book.
    pub half_tick_mid: Option<i64>,
    pub trace: Option<VolTrace>,
}

/// What rides on a [`SessionFrame`]: called synchronously at the frame's
/// single print point and its single close point, so nothing the frame
/// measures can bypass it.
pub trait FrameObserver {
    /// A measured print, after the session rotation it may have forced.
    fn on_print(&mut self, ts: u64, price_nanos: i64) -> LabResult<()>;
    /// A measured parent, after it entered its session.
    fn on_measured_parent(&mut self, parent: &ClosedParent) -> LabResult<()>;
}

/// The observer that observes nothing: the frame's session blocks alone.
impl FrameObserver for () {
    fn on_print(&mut self, _ts: u64, _price_nanos: i64) -> LabResult<()> {
        Ok(())
    }
    fn on_measured_parent(&mut self, _parent: &ClosedParent) -> LabResult<()> {
        Ok(())
    }
}

/// How a complete session is reduced to its emitted record.
pub(crate) type SessionCloser = fn(SessionAcc, Scope) -> LabResult<serde_json::Value>;

/// The session half of a generated measurement: which prints and parents are
/// measured, session rotation, and complete-session emission through the
/// unified [`SessionAcc`]. An accumulator is terminal once any call refuses.
pub(crate) struct SessionFrame<O> {
    seed: u64,
    start: u64,
    end: u64,
    offset: i32,
    segments: SegmentCursor,
    tick_nanos: i64,
    session: Option<SessionAcc>,
    sessions_out: Vec<serde_json::Value>,
    open_parent: Option<OpenParent>,
    /// The parent path's book in force: the last quote read, in nano units.
    /// Never taken, because a parent need not publish a quote of its own.
    book_in_force: Option<(i64, i64)>,
    count_windows_s: &'static [i64],
    close_with: SessionCloser,
    pub(crate) observer: O,
}

impl<O: FrameObserver> SessionFrame<O> {
    #[expect(clippy::too_many_arguments, reason = "the frame's full exposure")]
    pub(crate) fn new(
        seed: u64,
        start: u64,
        end: u64,
        offset: i32,
        tick_nanos: i64,
        count_windows_s: &'static [i64],
        close_with: SessionCloser,
        observer: O,
    ) -> Self {
        Self {
            seed,
            start,
            end,
            offset,
            segments: SegmentCursor::new(offset),
            tick_nanos,
            session: None,
            sessions_out: Vec::new(),
            open_parent: None,
            book_in_force: None,
            count_windows_s,
            close_with,
            observer,
        }
    }

    pub(crate) fn end(&self) -> u64 {
        self.end
    }

    /// Fold one whole parent. The view is the boundary: any parent still open
    /// from an earlier view is closed first, and at most one parent opens per
    /// view, at its first sided trade, against the book in force and the
    /// view's own trace. An unsided print closes the open parent and does not
    /// clear the view's one opening, so nothing reopens after it.
    pub(crate) fn push_parent(
        &mut self,
        scale: &RecordScale,
        view: &UnsizedParentView<'_>,
    ) -> LabResult<()> {
        self.close_open_parent()?;
        let dp = scale.price_dp();
        let mut opened = false;
        for record in view.records() {
            match record {
                UnsizedRecord::Quote { bid_px, ask_px, .. } => {
                    self.book_in_force =
                        Some((mantissa_nanos(bid_px, dp)?, mantissa_nanos(ask_px, dp)?));
                }
                UnsizedRecord::Trade { ts, px, side } => {
                    self.print(ts, || mantissa_nanos(px, dp))?;
                    if matches!(side, AggressorSide::NoAggressor) {
                        self.close_open_parent()?;
                        continue;
                    }
                    if !opened && self.open_parent.is_none() {
                        opened = true;
                        let Some((bid, ask)) = self.book_in_force else {
                            continue; // a pre-first-quote trade: no book
                        };
                        self.open(ts, bid, ask, view.trace);
                    }
                }
            }
        }
        Ok(())
    }

    fn has_open_parent(&self) -> bool {
        self.open_parent.is_some()
    }

    fn open(&mut self, first_ts: u64, bid_nanos: i64, ask_nanos: i64, trace: Option<VolTrace>) {
        self.open_parent = Some(OpenParent {
            first_ts,
            bid_nanos,
            ask_nanos,
            normal_book: bid_nanos < ask_nanos && bid_nanos > 0,
            trace,
        });
    }

    /// The per-print half of a trade: session rotation and the in-window print
    /// accumulation. `price` is converted only for a measured print, so an
    /// off-grid print outside the window refuses nothing.
    fn print(&mut self, ts: u64, price: impl FnOnce() -> LabResult<i64>) -> LabResult<()> {
        // Measured means inside the window and inside a segment of the lab's
        // session frame. The frame carves a 15:15 to 15:30 Chicago halt that
        // the exchange does not have (its status feed shows none, and the
        // venue's MNQ calendar stopped carving it at tape protocol 31), so a
        // generated print can now land in a minute the frame calls closed.
        // The observed pass drops the real prints in that minute because they
        // resolve to no usable session; this pass drops the generated ones
        // for the same reason, so the two sides of the measurement exclude
        // the same fifteen minutes rather than one side refusing the tape.
        if ts < self.start || ts >= self.end {
            return Ok(());
        }
        let Some(seg) = self.segments.at(ts) else {
            return Ok(());
        };
        // Session rotation on the trade's own instant: the outgoing parent and
        // session close before the incoming session exists.
        let rotate = self
            .session
            .as_ref()
            .is_none_or(|s| s.session_start_ns != seg.session_start_ns);
        if rotate {
            self.close_open_parent()?;
            self.close_session()?;
            self.session = Some(self.new_session(&seg));
        }
        let price = price()?;
        if let Some(session) = &mut self.session {
            session.push_print(ts, price);
        }
        self.observer.on_print(ts, price)
    }

    fn new_session(&self, seg: &SessionSegment) -> SessionAcc {
        SessionAcc::new_with_count_windows(
            crate::session::format_trade_date(seg.trade_day),
            seg,
            self.offset,
            self.count_windows_s,
        )
    }

    /// The single close point. A measured parent enters its session and then
    /// reaches the observer; an unmeasured one is dropped here.
    fn close_open_parent(&mut self) -> LabResult<()> {
        let Some(parent) = self.open_parent.take() else {
            return Ok(());
        };
        // Measured parents only: the first child inside [start, end).
        if parent.first_ts < self.start || parent.first_ts >= self.end {
            return Ok(());
        }
        // A parent that opened in a minute the lab frame calls closed (its
        // 15:15 halt, which the exchange does not have; see `print`) is
        // unmeasured, exactly as one that opened outside the window is.
        let Some(seg) = self.segments.at(parent.first_ts) else {
            return Ok(());
        };
        let segment_index = u8::from(seg.segment_origin_ns != seg.session_start_ns);
        let sum = parent
            .bid_nanos
            .checked_add(parent.ask_nanos)
            .ok_or_else(|| LabError::refusal("a quote mid overflows i64 nano units".to_string()))?;
        let half_tick_mid = if parent.normal_book {
            Some(exact_div(sum, self.tick_nanos, "quote-mid half-ticks")?)
        } else {
            None
        };
        if let Some(session) = &mut self.session {
            if session.session_start_ns != seg.session_start_ns {
                return Err(LabError::refusal(format!(
                    "a measured parent at {} closes into session {} not {}; the rotation \
                     invariant is broken",
                    parent.first_ts, session.date, seg.session_start_ns
                )));
            }
            session.push_parent(
                segment_index,
                parent.first_ts,
                parent.bid_nanos,
                parent.ask_nanos,
                parent.normal_book,
            )?;
        }
        self.observer.on_measured_parent(&ClosedParent {
            first_ts: parent.first_ts,
            minute: parent.first_ts / NS_PER_MIN,
            segment_index,
            half_tick_mid,
            trace: parent.trace,
        })
    }

    fn close_session(&mut self) -> LabResult<()> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        // Complete sessions only: a session must lie fully inside
        // [start, end) to be emitted (the Q1 all-session rule - the
        // downstream ObsContext treats every supplied session as a vote).
        if session.session_start_ns < self.start || session.session_end_ns > self.end {
            return Ok(());
        }
        let value = (self.close_with)(session, Scope::Generated { seed: self.seed })?;
        self.sessions_out.push(value);
        Ok(())
    }

    /// Terminal flush: the open parent, then the open session. Returns the
    /// emitted sessions and the observer.
    pub(crate) fn finish(mut self) -> LabResult<(Vec<serde_json::Value>, O)> {
        self.close_open_parent()?;
        self.close_session()?;
        Ok((self.sessions_out, self.observer))
    }
}

/// The previous measured parent (first child in `[start, end)`), for
/// `sigma_start` and the deferred ARCH share. Burn-in parents never enter.
struct PrevParent {
    seq: u64,
    minute: u64,
    base_return: f64,
    sigma2_realized: f64,
    has_trace: bool,
}

/// The Block-5 forensic state. Walk-global: nothing here resets at a session
/// rotation, so a measured successor in the next session still resolves its
/// predecessor's deferred share.
pub(crate) struct Forensics {
    segments: SegmentCursor,
    minutes: BTreeMap<u64, MinuteRec>,
    open_minute: Option<u64>,
    prev_parent: Option<PrevParent>,
    parent_seq: u64,
}

impl Forensics {
    fn new(offset: i32) -> Self {
        Self {
            segments: SegmentCursor::new(offset),
            minutes: BTreeMap::new(),
            open_minute: None,
            prev_parent: None,
            parent_seq: 0,
        }
    }

    /// Get-or-create the forensic minute record. Minute closure (the
    /// initiation resolution) is driven by parent minutes advancing in
    /// `on_measured_parent`, never by trades.
    fn forensic_minute(&mut self, minute: u64, event_ts: u64) -> LabResult<&mut MinuteRec> {
        if !self.minutes.contains_key(&minute) {
            let seg = self
                .segments
                .at(minute * NS_PER_MIN)
                .or_else(|| self.segments.at(event_ts))
                .ok_or_else(|| {
                    LabError::refusal("an in-window event maps to no open segment".to_string())
                })?;
            let index = u8::from(seg.segment_origin_ns != seg.session_start_ns);
            self.minutes.insert(minute, MinuteRec::new(index));
        }
        Ok(self.minutes.get_mut(&minute).expect("inserted above"))
    }

    fn resolve_initiation(&mut self, minute: u64) {
        let Some(rec) = self.minutes.get_mut(&minute) else {
            return;
        };
        let final_range = match (rec.quote_lo, rec.quote_hi) {
            (Some(lo), Some(hi)) => hi - lo,
            // A child-only extreme must stay visible: false, never a refusal.
            _ => 0,
        };
        rec.initiation = if final_range > 0 && rec.traced > 0 {
            // The first instant the running range strictly exceeds half the
            // final value, on the exact half-tick grid: 2 * running > final.
            rec.breakpoints
                .iter()
                .find(|&&(_, running)| 2 * running > final_range)
                .is_some_and(|&(ts, _)| rec.largest_inn_ts <= ts)
        } else {
            false
        };
        rec.breakpoints = Vec::new();
    }

    /// Resolve the last open minute; called once, after the frame's flush.
    fn finish(&mut self) {
        if let Some(open) = self.open_minute.take() {
            self.resolve_initiation(open);
        }
    }
}

impl FrameObserver for Forensics {
    fn on_print(&mut self, ts: u64, price: i64) -> LabResult<()> {
        let rec = self.forensic_minute(ts / NS_PER_MIN, ts)?;
        rec.trade_count += 1;
        match (&mut rec.trade_lo, &mut rec.trade_hi) {
            (Some(lo), Some(hi)) => {
                if price < *lo {
                    *lo = price;
                }
                if price > *hi {
                    *hi = price;
                }
            }
            _ => {
                rec.trade_lo = Some(price);
                rec.trade_hi = Some(price);
            }
        }
        Ok(())
    }

    fn on_measured_parent(&mut self, parent: &ClosedParent) -> LabResult<()> {
        let minute = parent.minute;
        let seq = self.parent_seq;
        self.parent_seq += 1;
        let prev = self.prev_parent.take();
        // Resolve the previous parent's deferred ARCH share against this
        // parent's candidate sigma2 (the successor may lie in a later minute;
        // both parents must be measured and traced).
        if let (Some(p), Some(trace)) = (&prev, &parent.trace)
            && p.has_trace
        {
            let share = ARCH_12A * p.base_return * p.base_return / trace.sigma2_candidate;
            if share.is_finite()
                && let Some(rec) = self.minutes.get_mut(&p.minute)
            {
                if rec.largest_inn_seq == p.seq && rec.traced > 0 {
                    rec.arch_share_next = Some(share);
                }
                rec.arch_share_max = Some(match rec.arch_share_max {
                    Some(m) if m >= share => m,
                    _ => share,
                });
            }
        }
        // The parent-minute advance closes the previously open forensic
        // minute: no further parent can join it (first-child timestamps are
        // chronological), so its initiation is now resolvable.
        match self.open_minute {
            Some(open) if minute > open => {
                self.resolve_initiation(open);
                self.open_minute = Some(minute);
            }
            None => self.open_minute = Some(minute),
            _ => {}
        }
        let first_of_minute = self
            .minutes
            .get(&minute)
            .is_none_or(|r| r.parent_count == 0);
        let sigma_start = prev.as_ref().map(|p| p.sigma2_realized.sqrt());
        let bounds = innovation_bounds();
        let rec = self.forensic_minute(minute, parent.first_ts)?;
        rec.parent_count += 1;
        if first_of_minute {
            rec.sigma_start = sigma_start;
        }
        if let Some(mid2) = parent.half_tick_mid {
            let lo = rec.quote_lo.map_or(mid2, |v| v.min(mid2));
            let hi = rec.quote_hi.map_or(mid2, |v| v.max(mid2));
            let grew = rec.quote_lo != Some(lo) || rec.quote_hi != Some(hi);
            rec.quote_lo = Some(lo);
            rec.quote_hi = Some(hi);
            if grew {
                rec.breakpoints.push((parent.first_ts, hi - lo));
            }
        }
        if let Some(trace) = &parent.trace {
            rec.traced += 1;
            let inn = trace.innovation_std.abs();
            if inn > rec.largest_inn {
                rec.largest_inn = inn;
                rec.largest_inn_ts = parent.first_ts;
                rec.largest_inn_seq = seq;
                // Any deferred share resolved for the previous largest parent
                // no longer describes this minute's largest-innovation
                // parent: null until (and unless) the new largest gains a
                // measured successor.
                rec.arch_share_next = None;
            }
            for (slot, bound) in rec.exceed.iter_mut().zip(bounds) {
                if inn > bound {
                    *slot += 1;
                }
            }
            let sigma = trace.sigma2_realized.sqrt();
            if sigma > rec.sigma_peak {
                rec.sigma_peak = sigma;
            }
            rec.sigma_end = sigma;
            for mid in [trace.mid_before, trace.mid_after] {
                if mid < rec.latent_lo {
                    rec.latent_lo = mid;
                }
                if mid > rec.latent_hi {
                    rec.latent_hi = mid;
                }
            }
            // clamp_hits: the sum of the three Boolean flag occurrences - a
            // parent with two simultaneous flags contributes two.
            rec.clamp_hits += u64::from(trace.sigma_cap_hit)
                + u64::from(trace.feedback_clamp_hit)
                + u64::from(trace.realized_clamp_hit);
            let sign = if trace.realized_return > 0.0 {
                1i8
            } else if trace.realized_return < 0.0 {
                -1i8
            } else {
                0i8
            };
            if sign != 0 && sign == rec.cur_sign {
                rec.cur_run += 1;
            } else if sign != 0 {
                rec.cur_sign = sign;
                rec.cur_run = 1;
            } else {
                rec.cur_sign = 0;
                rec.cur_run = 0;
            }
            if rec.cur_run > rec.max_signed_run {
                rec.max_signed_run = rec.cur_run;
            }
        }
        self.prev_parent = Some(PrevParent {
            seq,
            minute,
            base_return: parent.trace.as_ref().map_or(f64::NAN, |t| t.base_return),
            sigma2_realized: parent
                .trace
                .as_ref()
                .map_or(f64::NAN, |t| t.sigma2_realized),
            has_trace: parent.trace.is_some(),
        });
        Ok(())
    }
}

/// The generated-side accumulator: session blocks 1-4 through the shared
/// [`SessionFrame`], plus the Block-5 forensic minute records.
pub struct GeneratedAcc {
    seed: u64,
    tick_f: f64,
    frame: SessionFrame<Forensics>,
    /// The event path's pending book: set by a quote, taken by the parent it
    /// opens.
    pending_quote: Option<(i64, i64, Option<VolTrace>)>,
}

impl GeneratedAcc {
    /// `tick` is the instrument's modal tick; `[start, end)` is the measured
    /// window (the walk itself begins earlier, at `start - burn_in`).
    #[must_use]
    pub fn new(seed: u64, start: u64, end: u64, offset: i32, tick: Decimal) -> Self {
        Self::new_with_count_windows(
            seed,
            start,
            end,
            offset,
            tick,
            crate::subcontract::COUNT_WINDOWS_S,
        )
    }

    #[must_use]
    pub fn new_with_count_windows(
        seed: u64,
        start: u64,
        end: u64,
        offset: i32,
        tick: Decimal,
        count_windows_s: &'static [i64],
    ) -> Self {
        let (tick_nanos, tick_f) = tick_units(tick);
        Self {
            seed,
            tick_f,
            frame: SessionFrame::new(
                seed,
                start,
                end,
                offset,
                tick_nanos,
                count_windows_s,
                SessionAcc::close,
                Forensics::new(offset),
            ),
            pending_quote: None,
        }
    }

    /// Drive the accumulator over `source` a whole parent at a time, stopping
    /// before the first parent that begins at or past the measured window's
    /// end. A walk the source cut short by a fault refuses: exhaustion would
    /// otherwise read as a complete window and emit its sessions as measured.
    pub fn walk(&mut self, source: &mut dyn ParentSource) -> LabResult<()> {
        let scale = source.scale().clone();
        while let Some(view) = source.next_parent_unsized() {
            if view.first_ts() >= self.frame.end() {
                return Ok(());
            }
            self.frame.push_parent(&scale, &view)?;
        }
        match source.fault() {
            Some(fault) => Err(LabError::refusal(format!(
                "the generator faulted before the measured window's end: {fault:?}"
            ))),
            None => Ok(()),
        }
    }

    /// Fold one whole parent read at `scale`; see [`SessionFrame`] for how a
    /// view opens and closes parents.
    pub fn push_parent(
        &mut self,
        scale: &RecordScale,
        view: &UnsizedParentView<'_>,
    ) -> LabResult<()> {
        self.frame.push_parent(scale, view)
    }

    /// A quote closes the parent that ran under the previous book and becomes
    /// the pending book for the next one. The event-shaped entry; see the
    /// module documentation.
    pub fn push_quote(&mut self, q: &QuoteTick, trace: Option<VolTrace>) -> LabResult<()> {
        self.frame.close_open_parent()?;
        self.pending_quote = Some((price_nanos(q.bid_px)?, price_nanos(q.ask_px)?, trace));
        Ok(())
    }

    pub fn push_trade(&mut self, t: &TradeTick) -> LabResult<()> {
        self.frame.print(t.ts_event, || price_nanos(t.price))?;
        // An unsided print terminates the parent, like the observed side.
        if matches!(t.aggressor, AggressorSide::NoAggressor) {
            self.frame.close_open_parent()?;
            return Ok(());
        }
        if !self.frame.has_open_parent() {
            let Some((bid, ask, trace)) = self.pending_quote.take() else {
                return Ok(()); // a pre-first-quote trade: no book
            };
            self.frame.open(t.ts_event, bid, ask, trace);
        }
        Ok(())
    }

    /// Terminal flush plus forensic selection; consumes the accumulator.
    /// Returns `{seed, per_session, forensic}` - the `cost` field is the
    /// caller's, since only the caller knows the walk's wall time.
    pub fn finish(self) -> LabResult<serde_json::Value> {
        let (sessions, mut forensics) = self.frame.finish()?;
        forensics.finish();
        let (records, refusals) = select(&forensics.minutes, self.seed, self.tick_f)?;
        Ok(serde_json::json!({
            "seed": self.seed,
            "per_session": sessions,
            "forensic": {"records": records, "refusals": refusals},
        }))
    }
}
