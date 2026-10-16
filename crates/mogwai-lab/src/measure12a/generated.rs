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
//! exactly as an `N` row does on the observed side.
//!
//! [`GeneratedAcc::push_quote`] and [`GeneratedAcc::push_trade`] remain as
//! the event-shaped entry for hand-built streams with no parent divisions:
//! there a quote closes the open parent and stands in for the boundary.

use std::collections::BTreeMap;

use mogwai_data::{ParentSource, ParentView, Record, RecordScale, VolTrace};
use mogwai_protocol::{AggressorSide, QuoteTick, TradeTick};
use rust_decimal::Decimal;

use crate::error::{LabError, LabResult};
use crate::measure12a::forensic::{MinuteRec, innovation_bounds, select};
use crate::measure12a::{NS_PER_MIN, Scope, SessionAcc, exact_div};
use crate::session::{SessionSegment, session_segment_at};
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

/// The previous measured parent (first child in `[start, end)`), for
/// `sigma_start` and the deferred ARCH share. Burn-in parents never enter.
struct PrevParent {
    seq: u64,
    minute: u64,
    base_return: f64,
    sigma2_realized: f64,
    has_trace: bool,
}

struct OpenParent {
    first_ts: u64,
    bid_nanos: i64,
    ask_nanos: i64,
    normal_book: bool,
    trace: Option<VolTrace>,
}

/// The generated-side accumulator: session blocks 1-4 through the unified
/// [`SessionAcc`], plus the Block-5 forensic minute records.
pub struct GeneratedAcc {
    seed: u64,
    start: u64,
    end: u64,
    offset: i32,
    tick_nanos: i64,
    tick_f: f64,
    session: Option<SessionAcc>,
    sessions_out: Vec<serde_json::Value>,
    /// The event path's pending book: set by a quote, taken by the parent it
    /// opens.
    pending_quote: Option<(i64, i64, Option<VolTrace>)>,
    /// The parent path's book in force: the last quote read, in nano units.
    /// Never taken, because a parent need not publish a quote of its own.
    book_in_force: Option<(i64, i64)>,
    open_parent: Option<OpenParent>,
    minutes: BTreeMap<u64, MinuteRec>,
    open_minute: Option<u64>,
    prev_parent: Option<PrevParent>,
    parent_seq: u64,
    count_windows_s: &'static [i64],
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
        let tick_nanos = price_nanos(tick).unwrap_or(TICK_UNITS);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a tick is a small exact decimal"
        )]
        let tick_f = tick_nanos as f64 / 1e9;
        Self {
            seed,
            start,
            end,
            offset,
            tick_nanos,
            tick_f,
            session: None,
            sessions_out: Vec::new(),
            pending_quote: None,
            book_in_force: None,
            open_parent: None,
            minutes: BTreeMap::new(),
            open_minute: None,
            prev_parent: None,
            parent_seq: 0,
            count_windows_s,
        }
    }

    /// Drive the accumulator over `source` a whole parent at a time, stopping
    /// before the first parent that begins at or past the measured window's
    /// end. A walk the source cut short by a fault refuses: exhaustion would
    /// otherwise read as a complete window and emit its sessions as measured.
    pub fn walk(&mut self, source: &mut dyn ParentSource) -> LabResult<()> {
        let scale = source.scale().clone();
        while let Some(view) = source.next_parent() {
            if view.records[0].ts() >= self.end {
                return Ok(());
            }
            self.push_parent(&scale, &view)?;
        }
        match source.fault() {
            Some(fault) => Err(LabError::refusal(format!(
                "the generator faulted before the measured window's end: {fault:?}"
            ))),
            None => Ok(()),
        }
    }

    /// Fold one whole parent read at `scale`. The view is the boundary: any
    /// parent still open from an earlier view is closed first, and at most one
    /// parent opens per view, at its first sided trade, against the book in
    /// force and the view's own trace.
    pub fn push_parent(&mut self, scale: &RecordScale, view: &ParentView<'_>) -> LabResult<()> {
        self.close_open_parent()?;
        let dp = scale.price_dp();
        let mut opened = false;
        for record in view.records {
            match *record {
                Record::Quote(q) => {
                    self.book_in_force =
                        Some((mantissa_nanos(q.bid_px, dp)?, mantissa_nanos(q.ask_px, dp)?));
                }
                Record::Trade(t) => {
                    self.print(t.ts, || mantissa_nanos(t.px, dp))?;
                    if matches!(t.side, AggressorSide::NoAggressor) {
                        self.close_open_parent()?;
                        continue;
                    }
                    if !opened && self.open_parent.is_none() {
                        opened = true;
                        let Some((bid, ask)) = self.book_in_force else {
                            continue; // a pre-first-quote trade: no book
                        };
                        self.open_parent = Some(OpenParent {
                            first_ts: t.ts,
                            bid_nanos: bid,
                            ask_nanos: ask,
                            normal_book: bid < ask && bid > 0,
                            trace: view.trace,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// A quote closes the parent that ran under the previous book and becomes
    /// the pending book for the next one. The event-shaped entry; see the
    /// module documentation.
    pub fn push_quote(&mut self, q: &QuoteTick, trace: Option<VolTrace>) -> LabResult<()> {
        self.close_open_parent()?;
        self.pending_quote = Some((price_nanos(q.bid_px)?, price_nanos(q.ask_px)?, trace));
        Ok(())
    }

    pub fn push_trade(&mut self, t: &TradeTick) -> LabResult<()> {
        self.print(t.ts_event, || price_nanos(t.price))?;
        // An unsided print terminates the parent, like the observed side.
        if matches!(t.aggressor, AggressorSide::NoAggressor) {
            self.close_open_parent()?;
            return Ok(());
        }
        if self.open_parent.is_none() {
            let Some((bid, ask, trace)) = self.pending_quote.take() else {
                return Ok(()); // a pre-first-quote trade: no book
            };
            self.open_parent = Some(OpenParent {
                first_ts: t.ts_event,
                bid_nanos: bid,
                ask_nanos: ask,
                normal_book: bid < ask && bid > 0,
                trace,
            });
        }
        Ok(())
    }

    /// The per-print half of a trade, shared by both entries: session
    /// rotation and the in-window print and forensic-minute accumulation.
    /// `price` is converted only for a measured print, so an off-grid print
    /// outside the window refuses nothing.
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
        let in_window =
            ts >= self.start && ts < self.end && session_segment_at(ts, self.offset).is_some();
        // Session rotation on the trade's own instant.
        if in_window && let Some(seg) = session_segment_at(ts, self.offset) {
            let rotate = self
                .session
                .as_ref()
                .is_none_or(|s| s.session_start_ns != seg.session_start_ns);
            if rotate {
                self.close_open_parent()?;
                self.close_session()?;
                self.session = Some(self.new_session(&seg));
            }
        }
        if in_window {
            let price = price()?;
            let minute = ts / NS_PER_MIN;
            if let Some(session) = &mut self.session {
                session.push_print(ts, price);
            }
            let rec = self.forensic_minute(minute, ts)?;
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
        }
        Ok(())
    }

    fn new_session(&self, seg: &SessionSegment) -> SessionAcc {
        SessionAcc::new_with_count_windows(
            crate::session::format_trade_date(seg.trade_day),
            seg,
            self.offset,
            self.count_windows_s,
        )
    }

    /// Get-or-create the forensic minute record. Minute closure (the
    /// initiation resolution) is driven by parent minutes advancing in
    /// `close_open_parent`, never by trades.
    fn forensic_minute(&mut self, minute: u64, event_ts: u64) -> LabResult<&mut MinuteRec> {
        if !self.minutes.contains_key(&minute) {
            let seg = session_segment_at(minute * NS_PER_MIN, self.offset)
                .or_else(|| session_segment_at(event_ts, self.offset))
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

    fn close_open_parent(&mut self) -> LabResult<()> {
        let Some(parent) = self.open_parent.take() else {
            return Ok(());
        };
        // Measured parents only: the first child inside [start, end).
        if parent.first_ts < self.start || parent.first_ts >= self.end {
            return Ok(());
        }
        // A parent that opened in a minute the lab frame calls closed (its
        // 15:15 halt, which the exchange does not have; see `push_trade`) is
        // unmeasured, exactly as one that opened outside the window is.
        let Some(seg) = session_segment_at(parent.first_ts, self.offset) else {
            return Ok(());
        };
        let index = u8::from(seg.segment_origin_ns != seg.session_start_ns);
        let minute = parent.first_ts / NS_PER_MIN;
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
                index,
                parent.first_ts,
                parent.bid_nanos,
                parent.ask_nanos,
                parent.normal_book,
            )?;
        }
        // -- Forensic (Block 5) -------------------------------------------
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
        if let Some(mid2) = half_tick_mid {
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
        let value = session.close(Scope::Generated { seed: self.seed })?;
        self.sessions_out.push(value);
        Ok(())
    }

    /// Terminal flush plus forensic selection; consumes the accumulator.
    /// Returns `{seed, per_session, forensic}` - the `cost` field is the
    /// caller's, since only the caller knows the walk's wall time.
    pub fn finish(mut self) -> LabResult<serde_json::Value> {
        self.close_open_parent()?;
        self.close_session()?;
        if let Some(open) = self.open_minute.take() {
            self.resolve_initiation(open);
        }
        let (records, refusals) = select(&self.minutes, self.seed, self.tick_f)?;
        Ok(serde_json::json!({
            "seed": self.seed,
            "per_session": self.sessions_out,
            "forensic": {"records": records, "refusals": refusals},
        }))
    }
}
