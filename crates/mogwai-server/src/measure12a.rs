// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12a Brick G: the generated-side measurement consumer.
//!
//! `mogwai gen --type measure12a` runs one FINAL walk and emits the
//! per-seed JSON record `{seed, per_session, forensic, cost}` where each
//! per-session record carries EXACTLY the observed serialized shape the
//! Brick O harness (`analysis/mnq_fit.py`) emits: `{session_date,
//! segments, block1_hist, block2, block3, block4, permutations,
//! refusals}` with `permutations` always empty (spec 5.1 is
//! observed-side only). Blocks 1-4 port the frozen `_M12aSession`
//! conventions verbatim; Block 5 is the trace-grounded forensic record
//! set of spec 3.4b. The consumer only READS events and `VolTrace`
//! records - it neither adds nor changes any field, branch, callback,
//! buffer or draw in `GeneratedSource`, so `TAPE_PROTOCOL_VERSION`
//! stays 11 (spec 2.3).

use std::collections::BTreeMap;

use anyhow::{Context, bail};
use mogwai_protocol::{AggressorSide, QuoteTick, TradeTick};
use rust_decimal::Decimal;

use crate::r#gen::{SessionSegment, session_segment_at};

/// Local copies of the mogwai-data GARCH coefficients. `mogwai-data` is
/// untouchable in 12a (spec 2.3), so the values are duplicated here and
/// PINNED against the shipped recursion by
/// `arch_coefficients_match_the_shipped_recursion`, which recovers both
/// coefficients from consecutive `VolTrace` records of a real walk - a
/// drift in `consts.rs` fails that test without mogwai-data exporting
/// anything.
pub(crate) const ARCH_12A: f64 = 0.02;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "pinned by arch_coefficients_match_the_shipped_recursion; \
                  the runtime path consumes only ARCH_12A"
    )
)]
pub(crate) const GARCH_12A: f64 = 0.979;

/// Frozen constants of the 12a spec (section 7).
const CONTROL_TIE_BASE_SEED: u64 = 3_141_592_653_589_793_238;
const WALL_HORIZONS_S: [u64; 5] = [1, 5, 15, 60, 300];
const HORIZON_PAIRS: [(u64, u64); 4] = [(1, 5), (5, 15), (15, 60), (60, 300)];
const COUNT_WINDOWS_S: [u64; 3] = [1, 5, 60];
const RESIDUAL_WINDOW_NS: u64 = 300_000_000_000;
const RESIDUAL_MIN_HISTORY: usize = 1000;
const RESIDUAL_EXCEED_MULTIPLES: [f64; 3] = [4.0, 8.0, 16.0];
const INNOVATION_EXCEED_ABS: [f64; 3] = [4.0, 8.0, 16.0];
const TOP_MINUTE_EXCLUDE: usize = 32;

const NS_PER_MIN: u64 = 60_000_000_000;
const NS_PER_HOUR: u64 = 3_600_000_000_000;

/// Bit-identical to `crates/mogwai-protocol/src/seeds.rs` (which keeps
/// its copy private) and to the Python harness; pinned by
/// `splitmix_and_tuple_mix_match_the_frozen_vectors`.
pub(crate) const fn splitmix64(x: u64) -> u64 {
    let x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The 3.4a multi-field derivation: fold splitmix64 over the fields in
/// listed order.
pub(crate) fn tuple_mix(base: u64, fields: &[u64]) -> u64 {
    let mut x = base;
    for &value in fields {
        x = splitmix64(x ^ value);
    }
    x
}

// -- Exact segment-relative labels (spec 3.2) -------------------------------

const SINCE_OPEN_BIN_NAMES: [&str; 3] = ["0-300", "300-1800", "1800+"];
const UNTIL_CLOSE_BIN_NAMES: [&str; 3] = ["1800+", "300-1800", "0-300"];

fn segment_labels(
    minute_start_ns: u64,
    origin_ns: u64,
    end_ns: u64,
) -> (&'static str, &'static str) {
    let since_s = (minute_start_ns.saturating_sub(origin_ns)) / 1_000_000_000;
    let until_s = (end_ns.saturating_sub(minute_start_ns)) / 1_000_000_000;
    let since = if since_s < 300 {
        SINCE_OPEN_BIN_NAMES[0]
    } else if since_s < 1800 {
        SINCE_OPEN_BIN_NAMES[1]
    } else {
        SINCE_OPEN_BIN_NAMES[2]
    };
    let until = if until_s > 1800 {
        UNTIL_CLOSE_BIN_NAMES[0]
    } else if until_s > 300 {
        UNTIL_CLOSE_BIN_NAMES[1]
    } else {
        UNTIL_CLOSE_BIN_NAMES[2]
    };
    (since, until)
}

/// Civil date of a UTC-epoch day count (Howard Hinnant's algorithm),
/// formatted YYYY-MM-DD. No calendar dependency needed.
fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The session label: the trade date, i.e. the civil date of the session
/// CLOSE in calendar-local time - matching the Python harness inventory
/// labels.
fn session_date(session_end_ns: u64, offset_minutes: i16) -> String {
    let local = i128::from(session_end_ns) + i128::from(offset_minutes) * 60_000_000_000;
    // The close sits at 16:00 local, strictly inside its civil day.
    let days = (local.div_euclid(86_400_000_000_000)) as i64;
    civil_date(days)
}

// -- Exact-grid helpers (design defect 7: no f64 rounding of prices) --------

/// `value / unit` as an exact integer; an off-grid value refuses.
fn exact_ticks(value: Decimal, unit: Decimal, what: &str) -> anyhow::Result<i64> {
    let ratio = value / unit;
    let rounded = ratio.round();
    if ratio != rounded {
        bail!("off-grid {what}: {value} on unit {unit}");
    }
    rounded
        .try_into()
        .with_context(|| format!("{what} out of i64 range"))
}

// -- Nearest-rank helpers matching the harness conventions ------------------

fn nearest_rank_sorted(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let rank = ((q * n as f64).ceil() as usize).clamp(1, n);
    Some(sorted[rank - 1])
}

/// Weighted nearest rank over ascending (value, weight) pairs: the first
/// value whose cumulative weight reaches `q * total` (the literal 5.2
/// rule the harness freezes).
fn weighted_nearest_rank(pairs: &[(i64, u64)], q: f64) -> Option<i64> {
    let total: u64 = pairs.iter().map(|&(_, w)| w).sum();
    if total == 0 {
        return None;
    }
    let target = q * total as f64;
    let mut cum = 0u64;
    for &(v, w) in pairs {
        cum += w;
        if cum as f64 >= target {
            return Some(v);
        }
    }
    pairs.last().map(|&(v, _)| v)
}

// -- Per-session accumulation state (the _M12aSession port) -----------------

struct SegmentState {
    origin_ns: u64,
    end_ns: u64,
    parent_ts: Vec<u64>,
    mid_ts: Vec<u64>,
    mid_log: Vec<f64>,
}

struct SessionState {
    start_ns: u64,
    end_ns: u64,
    date: String,
    /// `[overnight, post_halt]`, both always present: an empty calendar
    /// segment still contributes its scheduled zero-count windows and
    /// never-established horizon chains.
    segments: [SegmentState; 2],
    /// minute -> (trade lo, trade hi) over ALL in-window prints.
    trade_min: BTreeMap<u64, (Decimal, Decimal)>,
    /// minute -> (lo, hi) quote-mid half-ticks, normal-book parents only.
    quote_min: BTreeMap<u64, (i64, i64)>,
    /// minute -> sided parent count by first-child timestamp.
    n_min: BTreeMap<u64, u64>,
}

impl SessionState {
    fn new(seg: &SessionSegment, offset: i16) -> Self {
        // The post-halt segment of the SAME session: derive from an
        // instant strictly inside it (one minute before the close).
        let post = session_segment_at(seg.session_end_ns - NS_PER_MIN, offset)
            .expect("the pre-close minute is inside the post-halt segment");
        let overnight = session_segment_at(seg.session_start_ns, offset)
            .expect("the open instant is inside the overnight segment");
        Self {
            start_ns: seg.session_start_ns,
            end_ns: seg.session_end_ns,
            date: session_date(seg.session_end_ns, offset),
            segments: [
                SegmentState {
                    origin_ns: overnight.segment_origin_ns,
                    end_ns: overnight.segment_end_ns,
                    parent_ts: Vec::new(),
                    mid_ts: Vec::new(),
                    mid_log: Vec::new(),
                },
                SegmentState {
                    origin_ns: post.segment_origin_ns,
                    end_ns: post.segment_end_ns,
                    parent_ts: Vec::new(),
                    mid_ts: Vec::new(),
                    mid_log: Vec::new(),
                },
            ],
            trade_min: BTreeMap::new(),
            quote_min: BTreeMap::new(),
            n_min: BTreeMap::new(),
        }
    }
}

// -- Forensic per-minute state (spec 3.4b / Block 5) ------------------------

struct MinuteRec {
    segment_index: u8,
    parent_count: u64,
    trade_count: u64,
    traced: u64,
    largest_inn: f64,
    largest_inn_ts: u64,
    /// Walk-global sequence number of the largest-innovation parent, so
    /// the deferred `arch_share_next` can find its successor.
    largest_inn_seq: u64,
    exceed: [u64; 3],
    sigma_start: Option<f64>,
    sigma_peak: f64,
    sigma_end: f64,
    latent_lo: f64,
    latent_hi: f64,
    max_signed_run: u64,
    cur_run: u64,
    cur_sign: i8,
    clamp_hits: u64,
    arch_share_next: Option<f64>,
    arch_share_max: Option<f64>,
    /// (parent ts, running quote-mid range half-ticks) breakpoints,
    /// retained only while the minute is OPEN; initiation resolves at
    /// minute close and the vector is dropped.
    breakpoints: Vec<(u64, i64)>,
    quote_lo: Option<i64>,
    quote_hi: Option<i64>,
    trade_lo: Option<Decimal>,
    trade_hi: Option<Decimal>,
    initiation: bool,
}

impl MinuteRec {
    fn new(segment_index: u8) -> Self {
        Self {
            segment_index,
            parent_count: 0,
            trade_count: 0,
            traced: 0,
            largest_inn: f64::NEG_INFINITY,
            largest_inn_ts: 0,
            largest_inn_seq: 0,
            exceed: [0; 3],
            sigma_start: None,
            sigma_peak: 0.0,
            sigma_end: 0.0,
            latent_lo: f64::INFINITY,
            latent_hi: f64::NEG_INFINITY,
            max_signed_run: 0,
            cur_run: 0,
            cur_sign: 0,
            clamp_hits: 0,
            arch_share_next: None,
            arch_share_max: None,
            breakpoints: Vec::new(),
            quote_lo: None,
            quote_hi: None,
            trade_lo: None,
            trade_hi: None,
            initiation: false,
        }
    }
}

/// The previous MEASURED parent (first child in `[start, end)`), for
/// `sigma_start` and the deferred ARCH share. Warmup parents never enter
/// (design defect 3).
struct PrevParent {
    seq: u64,
    minute: u64,
    base_return: f64,
    sigma2_realized: f64,
    has_trace: bool,
}

struct OpenParent {
    first_ts: u64,
    bid_px: Decimal,
    ask_px: Decimal,
    normal_book: bool,
    trace: Option<mogwai_data::VolTrace>,
}

pub(crate) struct Measure12aAcc {
    seed: u64,
    start: u64,
    end: u64,
    offset: i16,
    tick: Decimal,
    tick_f: f64,
    session: Option<SessionState>,
    sessions_out: Vec<serde_json::Value>,
    pending_quote: Option<(Decimal, Decimal, Option<mogwai_data::VolTrace>)>,
    open_parent: Option<OpenParent>,
    minutes: BTreeMap<u64, MinuteRec>,
    open_minute: Option<u64>,
    prev_parent: Option<PrevParent>,
    parent_seq: u64,
}

impl Measure12aAcc {
    pub(crate) fn new(seed: u64, start: u64, end: u64, offset: i16, tick: Decimal) -> Self {
        Self {
            seed,
            start,
            end,
            offset,
            tick,
            tick_f: f64::try_from(tick).unwrap_or(f64::NAN),
            session: None,
            sessions_out: Vec::new(),
            pending_quote: None,
            open_parent: None,
            minutes: BTreeMap::new(),
            open_minute: None,
            prev_parent: None,
            parent_seq: 0,
        }
    }

    pub(crate) fn push_quote(
        &mut self,
        q: &QuoteTick,
        trace: Option<mogwai_data::VolTrace>,
    ) -> anyhow::Result<()> {
        // A quote closes the parent that ran under the PREVIOUS book.
        self.close_open_parent()?;
        self.pending_quote = Some((q.bid_px, q.ask_px, trace));
        Ok(())
    }

    pub(crate) fn push_trade(&mut self, t: &TradeTick) -> anyhow::Result<()> {
        let in_window = t.ts_event >= self.start && t.ts_event < self.end;
        // Session rotation on the trade's own instant.
        if in_window && let Some(seg) = session_segment_at(t.ts_event, self.offset) {
            let rotate = match &self.session {
                Some(s) => s.start_ns != seg.session_start_ns,
                None => true,
            };
            if rotate {
                self.close_open_parent()?;
                self.close_session()?;
                self.session = Some(SessionState::new(&seg, self.offset));
            }
        }
        if in_window {
            // Trade range over ALL prints, by the trade's own timestamp.
            let minute = t.ts_event / NS_PER_MIN;
            if let Some(session) = &mut self.session {
                let entry = session
                    .trade_min
                    .entry(minute)
                    .or_insert((t.price, t.price));
                if t.price < entry.0 {
                    entry.0 = t.price;
                }
                if t.price > entry.1 {
                    entry.1 = t.price;
                }
            }
            let rec = self.forensic_minute(minute, t.ts_event)?;
            rec.trade_count += 1;
            match (&mut rec.trade_lo, &mut rec.trade_hi) {
                (Some(lo), Some(hi)) => {
                    if t.price < *lo {
                        *lo = t.price;
                    }
                    if t.price > *hi {
                        *hi = t.price;
                    }
                }
                _ => {
                    rec.trade_lo = Some(t.price);
                    rec.trade_hi = Some(t.price);
                }
            }
        }
        // An unsided print terminates the parent like the observed side.
        if matches!(t.aggressor, AggressorSide::NoAggressor) {
            self.close_open_parent()?;
            return Ok(());
        }
        match &mut self.open_parent {
            Some(_) => {}
            None => {
                let Some((bid, ask, trace)) = self.pending_quote.take() else {
                    return Ok(()); // pre-first-quote trade: no book
                };
                self.open_parent = Some(OpenParent {
                    first_ts: t.ts_event,
                    bid_px: bid,
                    ask_px: ask,
                    normal_book: bid < ask && bid > Decimal::ZERO,
                    trace,
                });
            }
        }
        Ok(())
    }

    /// Get-or-create the forensic minute record. Minute CLOSURE (the
    /// initiation resolution) is driven by PARENT minutes advancing in
    /// `close_open_parent`, never by trades: a burst's later children
    /// can cross the minute boundary before their parent finalizes, and
    /// closing on the trade would resolve initiation without that
    /// parent's quote extrema and breakpoint.
    fn forensic_minute(&mut self, minute: u64, event_ts: u64) -> anyhow::Result<&mut MinuteRec> {
        if !self.minutes.contains_key(&minute) {
            let seg = session_segment_at(minute * NS_PER_MIN, self.offset)
                .or_else(|| session_segment_at(event_ts, self.offset))
                .context("an in-window event maps to no open segment")?;
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
            // A child-only extreme must stay visible: FALSE, never a
            // refusal (spec Block 5).
            _ => 0,
        };
        rec.initiation = if final_range > 0 && rec.traced > 0 {
            // First instant the running range STRICTLY exceeds half the
            // final value, on the exact half-tick grid: 2 * running >
            // final.
            rec.breakpoints
                .iter()
                .find(|&&(_, running)| 2 * running > final_range)
                .is_some_and(|&(ts, _)| rec.largest_inn_ts <= ts)
        } else {
            false
        };
        rec.breakpoints = Vec::new();
    }

    fn close_open_parent(&mut self) -> anyhow::Result<()> {
        let Some(parent) = self.open_parent.take() else {
            return Ok(());
        };
        // Measured parents only: first child inside [start, end).
        if parent.first_ts < self.start || parent.first_ts >= self.end {
            return Ok(());
        }
        let Some(seg) = session_segment_at(parent.first_ts, self.offset) else {
            bail!(
                "a measured parent at {} maps to no open segment",
                parent.first_ts
            );
        };
        let index = usize::from(seg.segment_origin_ns != seg.session_start_ns);
        let minute = parent.first_ts / NS_PER_MIN;
        let half_tick_mid = if parent.normal_book {
            Some(exact_ticks(
                parent.bid_px + parent.ask_px,
                self.tick,
                "quote-mid half-ticks",
            )?)
        } else {
            None
        };
        if let Some(session) = &mut self.session {
            if session.start_ns != seg.session_start_ns {
                bail!(
                    "a measured parent at {} closes into session {} not {}; \
                     the rotation invariant is broken",
                    parent.first_ts,
                    session.date,
                    seg.session_start_ns
                );
            }
            let segment = &mut session.segments[index];
            segment.parent_ts.push(parent.first_ts);
            *session.n_min.entry(minute).or_insert(0) += 1;
            if let Some(mid2) = half_tick_mid {
                let entry = session.quote_min.entry(minute).or_insert((mid2, mid2));
                if mid2 < entry.0 {
                    entry.0 = mid2;
                }
                if mid2 > entry.1 {
                    entry.1 = mid2;
                }
                // The CANONICAL log-mid arithmetic shared with the
                // Python observed side (which logs integer 1e-9 price
                // units): sum the two prices as exact integer nano
                // units, halve in f64, then ln. Log differences are not
                // bit-invariant under input rescaling, so both sides
                // must feed ln the identical f64.
                let sum_nanos = exact_ticks(
                    parent.bid_px + parent.ask_px,
                    Decimal::new(1, 9),
                    "price nano units",
                )?;
                segment.mid_ts.push(parent.first_ts);
                segment.mid_log.push((sum_nanos as f64 / 2.0).ln());
            }
        }
        // -- Forensic (Block 5) -------------------------------------------
        let seq = self.parent_seq;
        self.parent_seq += 1;
        let prev = self.prev_parent.take();
        // Resolve the PREVIOUS parent's deferred ARCH share against this
        // parent's candidate sigma2 (the successor may lie in a later
        // minute; both parents must be measured and traced).
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
        // minute: no further parent can join it (first-child timestamps
        // are chronological), so its initiation is now resolvable.
        match self.open_minute {
            Some(open) if minute > open => {
                self.resolve_initiation(open);
                self.open_minute = Some(minute);
            }
            None => self.open_minute = Some(minute),
            _ => {}
        }
        let first_of_minute = !self.minutes.contains_key(&minute)
            || self
                .minutes
                .get(&minute)
                .is_some_and(|r| r.parent_count == 0);
        let sigma_start = prev.as_ref().map(|p| p.sigma2_realized.sqrt());
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
                // Any deferred share resolved for the PREVIOUS largest
                // parent no longer describes this minute's
                // largest-innovation parent: null until (and unless)
                // the new largest gains a measured successor.
                rec.arch_share_next = None;
            }
            for (slot, bound) in rec.exceed.iter_mut().zip(INNOVATION_EXCEED_ABS) {
                if inn > bound {
                    *slot += 1;
                }
            }
            let sigma = trace.sigma2_realized.sqrt();
            if sigma > rec.sigma_peak {
                rec.sigma_peak = sigma;
            }
            rec.sigma_end = sigma;
            if trace.mid_before < rec.latent_lo {
                rec.latent_lo = trace.mid_before;
            }
            if trace.mid_after < rec.latent_lo {
                rec.latent_lo = trace.mid_after;
            }
            if trace.mid_before > rec.latent_hi {
                rec.latent_hi = trace.mid_before;
            }
            if trace.mid_after > rec.latent_hi {
                rec.latent_hi = trace.mid_after;
            }
            // clamp_hits: the SUM of the three Boolean flag occurrences -
            // a parent with two simultaneous flags contributes two.
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

    fn close_session(&mut self) -> anyhow::Result<()> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        // Only sessions fully contained in [start, end) are emitted (the
        // Q1 all-session rule; the downstream ObsContext treats every
        // supplied session as a vote).
        if session.start_ns < self.start || session.end_ns > self.end {
            return Ok(());
        }
        let value = serialize_session(&session, self.seed)?;
        self.sessions_out.push(value);
        Ok(())
    }

    /// Terminal flush plus forensic selection; consumes the accumulator.
    pub(crate) fn finish(
        mut self,
        walk_s: f64,
        rss_bytes: u64,
    ) -> anyhow::Result<serde_json::Value> {
        self.close_open_parent()?;
        self.close_session()?;
        if let Some(open) = self.open_minute.take() {
            self.resolve_initiation(open);
        }
        let (records, refusals) = select_forensics(&self.minutes, self.seed, self.tick_f)?;
        Ok(serde_json::json!({
            "seed": self.seed,
            "per_session": self.sessions_out,
            "forensic": {"records": records, "refusals": refusals},
            "cost": {"walk_s": walk_s, "rss_bytes": rss_bytes},
        }))
    }
}

// -- Serialization of the per-session blocks --------------------------------

fn serialize_session(s: &SessionState, seed: u64) -> anyhow::Result<serde_json::Value> {
    let (block4, refusals) = block4_map(s, seed)?;
    Ok(serde_json::json!({
        "session_date": s.date,
        "segments": [
            {"segment_index": 0, "open_ns": s.segments[0].origin_ns,
             "close_ns": s.segments[0].end_ns},
            {"segment_index": 1, "open_ns": s.segments[1].origin_ns,
             "close_ns": s.segments[1].end_ns},
        ],
        "block1_hist": block1_hist(s)?,
        "block2": block2_map(s),
        "block3": block3_map(s),
        "block4": block4,
        "permutations": [],
        "refusals": refusals,
    }))
}

fn block1_hist(s: &SessionState) -> anyhow::Result<serde_json::Value> {
    let mut hist: BTreeMap<(u64, i64, i64, u64, &str, &str), u64> = BTreeMap::new();
    let minutes: std::collections::BTreeSet<u64> =
        s.trade_min.keys().chain(s.n_min.keys()).copied().collect();
    for minute in minutes {
        let start_ns = minute * NS_PER_MIN;
        // Which segment: labels evaluate at MINUTE START.
        let seg = s
            .segments
            .iter()
            .find(|g| start_ns >= g.origin_ns && start_ns < g.end_ns)
            .with_context(|| format!("minute {minute} maps to no open segment of {}", s.date))?;
        let (since, until) = segment_labels(start_ns, seg.origin_ns, seg.end_ns);
        let hour = (start_ns / NS_PER_HOUR) % 24;
        let trade_ticks = match s.trade_min.get(&minute) {
            Some(&(lo, hi)) => exact_trade_ticks(lo, hi)?,
            None => 0,
        };
        let quote_half = s.quote_min.get(&minute).map_or(-1, |&(lo, hi)| hi - lo);
        let n = s.n_min.get(&minute).copied().unwrap_or(0);
        *hist
            .entry((n, quote_half, trade_ticks, hour, since, until))
            .or_insert(0) += 1;
    }
    // Ascending by (n, quote with null first, trade, hour, since, until):
    // the BTreeMap key order already matches because null encodes as -1.
    let rows: Vec<serde_json::Value> = hist
        .iter()
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
        .collect();
    Ok(serde_json::Value::Array(rows))
}

// Trade ranges must stay exact (design defect 7). The tick unit is
// threaded through thread-local state rather than every signature; set
// once per accumulator run.
thread_local! {
    static TRADE_TICK: std::cell::Cell<Option<(i64, u32)>> = const { std::cell::Cell::new(None) };
}

/// Stores the tick as (mantissa, scale) for exact integer division.
fn set_trade_tick(tick: Decimal) {
    TRADE_TICK.with(|c| c.set(Some((tick.mantissa() as i64, tick.scale()))));
}

fn exact_trade_ticks(lo: Decimal, hi: Decimal) -> anyhow::Result<i64> {
    let (mantissa, scale) = TRADE_TICK
        .with(std::cell::Cell::get)
        .context("trade tick unit not set")?;
    let tick = Decimal::from_i128_with_scale(i128::from(mantissa), scale);
    exact_ticks(hi - lo, tick, "trade range ticks")
}

fn block2_map(s: &SessionState) -> serde_json::Value {
    #[derive(Default)]
    struct Cell {
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
    let mut cells: BTreeMap<(u64, u64), Cell> = BTreeMap::new();
    for seg in &s.segments {
        for &w in &COUNT_WINDOWS_S {
            let w_ns = w * 1_000_000_000;
            let mut i = 0usize;
            let pts = &seg.parent_ts;
            let mut prev_count: Option<u64> = None;
            let mut prev_hour: Option<u64> = None;
            let mut run = 0u64;
            let close_run =
                |run: &mut u64, prev_hour: Option<u64>, cells: &mut BTreeMap<(u64, u64), Cell>| {
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
                };
            let mut start = seg.origin_ns;
            while start + w_ns <= seg.end_ns {
                let stop = start + w_ns;
                let s_hour = (start / NS_PER_HOUR) % 24;
                let e_hour = (stop / NS_PER_HOUR) % 24;
                while i < pts.len() && pts[i] < start {
                    i += 1;
                }
                let mut j = i;
                while j < pts.len() && pts[j] < stop {
                    j += 1;
                }
                let count = (j - i) as u64;
                i = j;
                if s_hour != e_hour {
                    // Hour-crossing (including exactly-on-boundary ends):
                    // excluded; runs and pairs reset.
                    close_run(&mut run, prev_hour, &mut cells);
                    prev_count = None;
                    prev_hour = None;
                    start = stop;
                    continue;
                }
                if prev_hour.is_some() && prev_hour != Some(e_hour) {
                    close_run(&mut run, prev_hour, &mut cells);
                    prev_count = None;
                }
                let cell = cells.entry((e_hour, w)).or_default();
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
                prev_hour = Some(e_hour);
                if count > 0 {
                    run += 1;
                } else {
                    close_run(&mut run, prev_hour, &mut cells);
                }
                start = stop;
            }
            close_run(&mut run, prev_hour, &mut cells);
        }
    }
    let mut out = serde_json::Map::new();
    for (&(hour, w), cell) in &cells {
        let total: u64 = cell.count_hist.values().sum();
        let ssum: u64 = cell.count_hist.iter().map(|(&k, &v)| k * v).sum();
        let ssq: u64 = cell.count_hist.iter().map(|(&k, &v)| k * k * v).sum();
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
        let n = cell.paired;
        let lag1 = if n >= 2 {
            let nf = n as f64;
            let vx = cell.sumsq_x as f64 - (cell.sum_x as f64).powi(2) / nf;
            let vy = cell.sumsq_y as f64 - (cell.sum_y as f64).powi(2) / nf;
            if vx > 0.0 && vy > 0.0 {
                Some(
                    (cell.sum_xy as f64 - cell.sum_x as f64 * cell.sum_y as f64 / nf)
                        / (vx * vy).sqrt(),
                )
            } else {
                None
            }
        } else {
            None
        };
        let count_pairs: Vec<(i64, u64)> = cell
            .count_hist
            .iter()
            .map(|(&k, &v)| (k as i64, v))
            .collect();
        let run_pairs: Vec<(i64, u64)> =
            cell.run_hist.iter().map(|(&k, &v)| (k as i64, v)).collect();
        let value = serde_json::json!({
            "scheduled_windows": cell.scheduled,
            "zero_windows": cell.zeros,
            "count_hist": cell.count_hist.iter()
                .map(|(&k, &v)| (k.to_string(), serde_json::json!(v)))
                .collect::<serde_json::Map<_, _>>(),
            "run_length_hist": cell.run_hist.iter()
                .map(|(&k, &v)| (k.to_string(), serde_json::json!(v)))
                .collect::<serde_json::Map<_, _>>(),
            "paired_lag_count": cell.paired,
            "sum_x": cell.sum_x, "sum_y": cell.sum_y,
            "sumsq_x": cell.sumsq_x, "sumsq_y": cell.sumsq_y,
            "sum_xy": cell.sum_xy,
            "zero_fraction": if cell.scheduled > 0 {
                serde_json::json!(cell.zeros as f64 / cell.scheduled as f64)
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
        });
        out.entry(hour.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .expect("hour entry is an object")
            .insert(w.to_string(), value);
    }
    serde_json::Value::Object(out)
}

/// One wall boundary: `(boundary, asof, emitted, hour, ret)`.
type Boundary = (u64, Option<f64>, bool, u64, Option<f64>);

/// The 4.6-convention wall boundaries over one segment's endpoint
/// series.
fn wall_boundaries(seg: &SegmentState, h_ns: u64) -> Vec<Boundary> {
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

fn block3_map(s: &SessionState) -> serde_json::Value {
    #[derive(Default, Clone, Copy)]
    struct Cell {
        n: u64,
        sum: f64,
        sumsq: f64,
        sum_abs: f64,
        max_abs: f64,
    }
    impl Cell {
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
    // (hour, (h, big)) -> (window count, sum of R_H^2, sum of comp^2).
    type PairAcc = BTreeMap<(u64, (u64, u64)), (u64, f64, f64)>;
    let mut cells: BTreeMap<(u64, u64), Cell> = BTreeMap::new();
    let mut pairs: PairAcc = BTreeMap::new();
    let mut h20: BTreeMap<(String, u64), Cell> = BTreeMap::new();
    let mut lag1: BTreeMap<u64, (u64, f64, f64, f64, f64, f64)> = BTreeMap::new();

    for seg in &s.segments {
        let series: BTreeMap<u64, Vec<Boundary>> = WALL_HORIZONS_S
            .iter()
            .map(|&h| (h, wall_boundaries(seg, h * 1_000_000_000)))
            .collect();
        type AsofAt = BTreeMap<u64, BTreeMap<u64, Option<f64>>>;
        let asof_at: AsofAt = series
            .iter()
            .map(|(&h, rows)| (h, rows.iter().map(|&(b, a, ..)| (b, a)).collect()))
            .collect();
        for &h in &WALL_HORIZONS_S {
            for &(b, _asof, emitted, hour, ret) in &series[&h] {
                if !emitted {
                    continue;
                }
                let r = ret.expect("emitted boundary carries a return");
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
                let big_r = ret.expect("emitted boundary carries a return");
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
        // Lag-1 parent-return scalar, attributed by the LATER return's
        // endpoint hour.
        let mut prev_r: Option<f64> = None;
        for idx in 1..seg.mid_ts.len() {
            let r = seg.mid_log[idx] - seg.mid_log[idx - 1];
            if let Some(p) = prev_r {
                let hour = (seg.mid_ts[idx] / NS_PER_HOUR) % 24;
                let acc = lag1.entry(hour).or_insert((0, 0.0, 0.0, 0.0, 0.0, 0.0));
                acc.0 += 1;
                acc.1 += p;
                acc.2 += r;
                acc.3 += p * p;
                acc.4 += r * r;
                acc.5 += p * r;
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
                        serde_json::json!((sum_rh2 - sum_comp2) / n as f64)
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
    let corr = |acc: &(u64, f64, f64, f64, f64, f64)| -> Option<f64> {
        let (n, sx, sy, sxx, syy, sxy) = *acc;
        if n < 2 {
            return None;
        }
        let nf = n as f64;
        let vx = sxx - sx * sx / nf;
        let vy = syy - sy * sy / nf;
        if vx <= 0.0 || vy <= 0.0 {
            return None;
        }
        Some((sxy - sx * sy / nf) / (vx * vy).sqrt())
    };
    let mut h20_out = serde_json::Map::new();
    for ((labels, h), cell) in &h20 {
        h20_out
            .entry(labels.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .expect("label entry is an object")
            .insert(h.to_string(), cell.json());
    }
    serde_json::json!({
        "cells": cells_out,
        "pairs": pairs_out,
        "lag1_parent_autocorr": lag1.iter()
            .map(|(&hour, acc)| (hour.to_string(), match corr(acc) {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            }))
            .collect::<serde_json::Map<_, _>>(),
        "hour20_labels": h20_out,
    })
}

/// Block 4 plus the Amendment-F standardizer-omission refusals, one per
/// (session, hour), scope carrying the seed.
fn block4_map(
    s: &SessionState,
    seed: u64,
) -> anyhow::Result<(serde_json::Value, Vec<serde_json::Value>)> {
    #[derive(Default)]
    struct HourAcc {
        residual_count: u64,
        warmup_excluded: u64,
        zeros: u64,
        nz_abs: Vec<f64>,
        exceed: [u64; 3],
    }
    let mut per_hour: BTreeMap<String, HourAcc> = BTreeMap::new();
    let mut omitted: BTreeMap<u64, u64> = BTreeMap::new();
    for seg in &s.segments {
        let ts = &seg.mid_ts;
        let logmid = &seg.mid_log;
        let mut window: std::collections::VecDeque<(u64, f64)> = Default::default();
        let mut maxq: std::collections::VecDeque<f64> = Default::default();
        let mut run_sum = 0.0f64;
        for idx in 1..ts.len() {
            let t = ts[idx];
            let r = logmid[idx] - logmid[idx - 1];
            let hour = (t / NS_PER_HOUR) % 24;
            let lo = t.saturating_sub(RESIDUAL_WINDOW_NS);
            while let Some(&(old_ts, old_abs)) = window.front() {
                if old_ts >= lo {
                    break;
                }
                window.pop_front();
                run_sum -= old_abs;
                if maxq.front() == Some(&old_abs) {
                    maxq.pop_front();
                }
            }
            let count = window.len();
            if count < RESIDUAL_MIN_HISTORY {
                per_hour
                    .entry(hour.to_string())
                    .or_default()
                    .warmup_excluded += 1;
                per_hour
                    .entry("all".to_string())
                    .or_default()
                    .warmup_excluded += 1;
            } else {
                let mx = maxq.front().copied().unwrap_or(0.0);
                let scale = (run_sum - mx) / (count - 1) as f64;
                if !scale.is_finite() || scale <= 0.0 {
                    // Amendment F: the residual is OMITTED; the return
                    // still enters history below.
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
                        for (slot, bound) in acc.exceed.iter_mut().zip(RESIDUAL_EXCEED_MULTIPLES) {
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
    let mut out = serde_json::Map::new();
    for (key, acc) in &mut per_hour {
        acc.nz_abs.sort_by(f64::total_cmp);
        let rc = acc.residual_count;
        let p90 = nearest_rank_sorted(&acc.nz_abs, 0.90);
        let p99 = nearest_rank_sorted(&acc.nz_abs, 0.99);
        let p999 = nearest_rank_sorted(&acc.nz_abs, 0.999);
        // The ratio nulls follow the harness truthiness convention: a
        // zero or missing component gives null.
        let ratio = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(x), Some(y)) if x != 0.0 && y != 0.0 => serde_json::json!(x / y),
            _ => serde_json::Value::Null,
        };
        let frac = |num: u64| {
            if rc > 0 {
                serde_json::json!(num as f64 / rc as f64)
            } else {
                serde_json::Value::Null
            }
        };
        out.insert(
            key.clone(),
            serde_json::json!({
                "residual_count": rc,
                "warmup_excluded": acc.warmup_excluded,
                "zero_fraction": if rc > 0 {
                    serde_json::json!(acc.zeros as f64 / rc as f64)
                } else {
                    serde_json::Value::Null
                },
                "nz_abs_p90": p90, "nz_abs_p99": p99, "nz_abs_p999": p999,
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
                "scope": format!("seed {seed} session {}", s.date),
                "cell": format!("block4 hour {hour} standardizer"),
                "reason": format!(
                    "{n} residuals omitted: nonpositive or non-finite trailing scale"
                ),
            })
        })
        .collect();
    Ok((serde_json::Value::Object(out), refusals))
}

// -- Forensic selection (spec 3.4b) -----------------------------------------

fn select_forensics(
    minutes: &BTreeMap<u64, MinuteRec>,
    seed: u64,
    tick_f: f64,
) -> anyhow::Result<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
    // Populated minutes: at least one in-window child trade.
    let populated: Vec<(u64, &MinuteRec)> = minutes
        .iter()
        .filter(|(_, r)| r.trade_count > 0)
        .map(|(&m, r)| (m, r))
        .collect();
    let mut records = Vec::new();
    let mut refusals: Vec<serde_json::Value> = Vec::new();
    if populated.is_empty() {
        return Ok((records, refusals));
    }
    let trade_ticks = |r: &MinuteRec| -> anyhow::Result<i64> {
        match (r.trade_lo, r.trade_hi) {
            (Some(lo), Some(hi)) => exact_trade_ticks(lo, hi),
            _ => Ok(0),
        }
    };
    // Extreme by trade range, earlier minute on ties (matching
    // rank_top_minutes).
    let mut ranked: Vec<(i64, u64)> = Vec::new();
    for &(m, r) in &populated {
        ranked.push((trade_ticks(r)?, m));
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let extreme_range_minute = ranked[0].1;
    // Extreme by range / sqrt(N) over N >= 1, earlier minute on ties.
    let mut best_sqrt: Option<(f64, u64)> = None;
    for &(ticks, m) in &ranked {
        let n = minutes[&m].parent_count;
        if n == 0 {
            continue;
        }
        let v = ticks as f64 / (n as f64).sqrt();
        best_sqrt = match best_sqrt {
            Some((bv, bm)) if bv > v || (bv == v && bm < m) => Some((bv, bm)),
            _ => Some((v, m)),
        };
    }
    // Deduplicate: a shared minute emits ONCE as extreme_range.
    let mut extremes: Vec<(u64, &'static str)> = vec![(extreme_range_minute, "extreme_range")];
    if let Some((_, m)) = best_sqrt
        && m != extreme_range_minute
    {
        extremes.push((m, "extreme_sqrt"));
    }
    let top_exclude: std::collections::BTreeSet<u64> = ranked
        .iter()
        .take(TOP_MINUTE_EXCLUDE)
        .map(|&(_, m)| m)
        .collect();
    let extreme_set: std::collections::BTreeSet<u64> = extremes.iter().map(|&(m, _)| m).collect();

    // Per (segment, hour) trade-range medians over populated minutes
    // (nearest-rank median).
    let mut group_ranges: BTreeMap<(u8, u64), Vec<i64>> = BTreeMap::new();
    for &(m, r) in &populated {
        let hour = (m * NS_PER_MIN / NS_PER_HOUR) % 24;
        group_ranges
            .entry((r.segment_index, hour))
            .or_default()
            .push(trade_ticks(r)?);
    }
    for v in group_ranges.values_mut() {
        v.sort_unstable();
    }
    let median_of = |key: (u8, u64)| -> Option<i64> {
        let v = group_ranges.get(&key)?;
        let rank = v.len().div_ceil(2);
        Some(v[rank - 1])
    };

    for &(minute, kind) in &extremes {
        let rec = &minutes[&minute];
        // Fail closed rather than fabricate: every selected minute must
        // carry at least one traced parent (no frozen empty-set
        // convention exists for the trace-grounded fields).
        if rec.traced == 0 {
            bail!(
                "selected forensic minute {} has no traced parent; the schema \
                 has no empty-set convention - stop for an amendment",
                minute * NS_PER_MIN
            );
        }
        records.push(forensic_record(
            seed,
            kind,
            minute,
            rec,
            None,
            tick_f,
            &mut refusals,
        )?);
        // Control selection.
        let hour = (minute * NS_PER_MIN / NS_PER_HOUR) % 24;
        let median = median_of((rec.segment_index, hour));
        let n_e = rec.parent_count;
        let mut best: Option<(f64, u64, u64)> = None; // (dist, rank, minute)
        for &(m, r) in &populated {
            if m == minute || extreme_set.contains(&m) || top_exclude.contains(&m) {
                continue;
            }
            let m_hour = (m * NS_PER_MIN / NS_PER_HOUR) % 24;
            if r.segment_index != rec.segment_index || m_hour != hour {
                continue;
            }
            let Some(med) = median else { continue };
            if trade_ticks(r)? > med {
                continue;
            }
            let dist = ((r.parent_count as f64).ln_1p() - (n_e as f64).ln_1p()).abs();
            let rank = tuple_mix(
                CONTROL_TIE_BASE_SEED,
                &[seed, minute * NS_PER_MIN, m * NS_PER_MIN],
            );
            let candidate = (dist, rank, m);
            best = match best {
                Some(cur)
                    if cur.0 < candidate.0
                        || (cur.0 == candidate.0 && cur.1 < candidate.1)
                        || (cur.0 == candidate.0
                            && cur.1 == candidate.1
                            && cur.2 < candidate.2) =>
                {
                    Some(cur)
                }
                _ => Some(candidate),
            };
        }
        match best {
            Some((_, _, control_minute)) => {
                let control = &minutes[&control_minute];
                if control.traced == 0 {
                    bail!(
                        "selected control minute {} has no traced parent; stop for an amendment",
                        control_minute * NS_PER_MIN
                    );
                }
                records.push(forensic_record(
                    seed,
                    "control",
                    control_minute,
                    control,
                    Some(minute * NS_PER_MIN),
                    tick_f,
                    &mut refusals,
                )?);
            }
            None => refusals.push(serde_json::json!({
                "scope": format!("seed {seed} forensic"),
                "cell": format!("control for minute {}", minute * NS_PER_MIN),
                "reason": "no qualifying candidate control",
            })),
        }
    }
    Ok((records, refusals))
}

fn forensic_record(
    seed: u64,
    kind: &str,
    minute: u64,
    rec: &MinuteRec,
    matched: Option<u64>,
    tick_f: f64,
    refusals: &mut Vec<serde_json::Value>,
) -> anyhow::Result<serde_json::Value> {
    let minute_start = minute * NS_PER_MIN;
    let trade_ticks = match (rec.trade_lo, rec.trade_hi) {
        (Some(lo), Some(hi)) => exact_trade_ticks(lo, hi)?,
        _ => 0,
    };
    let quote_half = match (rec.quote_lo, rec.quote_hi) {
        (Some(lo), Some(hi)) => Some(hi - lo),
        _ => None,
    };
    let latent_ticks = if rec.latent_hi >= rec.latent_lo {
        (rec.latent_hi - rec.latent_lo) / tick_f
    } else {
        0.0
    };
    // Exactly ONE logical refusal per refused cell, even when the same
    // minute serves as the control for both extremes and this function
    // runs twice for it.
    let mut refuse_once = |cell: String, reason: &str| {
        let rec = serde_json::json!({
            "scope": format!("seed {seed} forensic"),
            "cell": cell,
            "reason": reason,
        });
        if !refusals.contains(&rec) {
            refusals.push(rec);
        }
    };
    // sigma_start: null with ONE refusal owning both it and the
    // dependent sigma_escalation when the minute opens the measured walk.
    let escalation = match rec.sigma_start {
        None => {
            refuse_once(
                format!("minute {minute_start} sigma_start"),
                "first measured parent has no predecessor; \
                 sigma_start and sigma_escalation refused",
            );
            None
        }
        Some(s) if s <= 0.0 => {
            refuse_once(
                format!("minute {minute_start} sigma_escalation"),
                "nonpositive sigma_start refuses the escalation",
            );
            None
        }
        Some(s) => Some(rec.sigma_peak / s),
    };
    // Ratio nulls from absent or zero denominators are defined
    // emptiness, never refusals.
    let trade_to_quote = quote_half
        .filter(|&q| q > 0)
        .map(|q| trade_ticks as f64 / (q as f64 / 2.0));
    let quote_to_latent = quote_half
        .filter(|_| latent_ticks > 0.0)
        .map(|q| (q as f64 / 2.0) / latent_ticks);
    Ok(serde_json::json!({
        "seed": seed,
        "kind": kind,
        "matched_extreme_minute_start": matched,
        "minute_start_ns": minute_start,
        "minute_end_ns": minute_start + NS_PER_MIN,
        "utc_hour": (minute_start / NS_PER_HOUR) % 24,
        "segment_index": rec.segment_index,
        "parent_count": rec.parent_count,
        "trade_count": rec.trade_count,
        "traced_parents": rec.traced,
        "largest_innovation_std": rec.largest_inn,
        "largest_innovation_ts_ns": rec.largest_inn_ts,
        "innovation_exceed_4": rec.exceed[0],
        "innovation_exceed_8": rec.exceed[1],
        "innovation_exceed_16": rec.exceed[2],
        "initiation": rec.initiation,
        "sigma_start": rec.sigma_start,
        "sigma_peak": rec.sigma_peak,
        "sigma_end": rec.sigma_end,
        "sigma_escalation": escalation,
        "latent_mid_range_ticks": latent_ticks,
        "quote_mid_range_half_ticks": quote_half,
        "trade_range_ticks": trade_ticks,
        "trade_to_quote_range_ratio": trade_to_quote,
        "quote_to_latent_range_ratio": quote_to_latent,
        "max_signed_run": rec.max_signed_run,
        "clamp_hits": rec.clamp_hits,
        "arch_share_next": rec.arch_share_next,
        "arch_share_minute_max": rec.arch_share_max,
    }))
}

/// Peak resident set of this process (VmHWM), for the per-seed cost
/// field. The Brick M gate samples the process TREE externally; this is
/// only the JSON record.
pub(crate) fn self_peak_rss_bytes() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: u64 = rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .unwrap_or(0);
            return kb * 1024;
        }
    }
    0
}

/// Prepare the exact-grid trade-tick unit for this run.
pub(crate) fn prepare_trade_tick(tick: Decimal) {
    // The trade range is whole ticks; the quote-mid range is half-ticks.
    // `exact_trade_ticks` divides by the full tick.
    set_trade_tick(tick);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_and_tuple_mix_match_the_frozen_vectors() {
        // The same vectors the Python harness selftest pins.
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(splitmix64(1), 0x910A_2DEC_8902_5CC1);
        assert_eq!(tuple_mix(7, &[1, 2]), splitmix64(splitmix64(7 ^ 1) ^ 2));
        assert_ne!(tuple_mix(7, &[2, 1]), tuple_mix(7, &[1, 2]));
    }

    #[test]
    fn session_dates_derive_the_trade_date() {
        assert_eq!(civil_date(0), "1970-01-01");
        // The July 1 session closes 2026-07-01T21:00Z (offset -300);
        // the label is the close's local civil day - the trade date.
        assert_eq!(session_date(1_782_939_600_000_000_000, -300), "2026-07-01");
    }

    // -- Crafted-fixture machinery -------------------------------------

    /// 2026-07-06T22:00Z, the July 7 session open (offset -300).
    const OPEN_NS: u64 = 1_783_375_200_000_000_000;
    /// 2026-07-07T21:00Z, the July 7 session close.
    const CLOSE_NS: u64 = 1_783_458_000_000_000_000;
    /// 2026-07-06T23:00Z, the fixture hour.
    const H23_NS: u64 = OPEN_NS + 3_600_000_000_000;
    const OFFSET: i16 = -300;

    fn tick() -> Decimal {
        Decimal::new(25, 2) // 0.25, the MNQ tick
    }

    fn px(level: i64) -> Decimal {
        Decimal::from(23_000) + tick() * Decimal::from(level)
    }

    fn quote(ts: u64, level: i64) -> QuoteTick {
        QuoteTick {
            symbol: "MNQ".to_string(),
            bid_px: px(level - 1),
            ask_px: px(level + 1),
            bid_sz: Decimal::ONE,
            ask_sz: Decimal::ONE,
            ts_event: ts,
        }
    }

    fn trade(ts: u64, level: i64) -> TradeTick {
        TradeTick {
            symbol: "MNQ".to_string(),
            price: px(level),
            size: Decimal::ONE,
            aggressor: AggressorSide::Buyer,
            ts_event: ts,
        }
    }

    #[allow(clippy::too_many_arguments, reason = "a test fixture constructor")]
    fn vt(
        innovation_std: f64,
        sigma2_candidate: f64,
        sigma2_realized: f64,
        base_return: f64,
        realized_return: f64,
        mid: f64,
        sigma_cap_hit: bool,
        feedback_clamp_hit: bool,
    ) -> mogwai_data::VolTrace {
        mogwai_data::VolTrace {
            innovation_raw: innovation_std * std::f64::consts::SQRT_2,
            innovation_std,
            sigma2_candidate,
            sigma2_realized,
            sigma_cap_hit,
            garch_scale: sigma2_realized.sqrt(),
            base_return_unclipped: base_return,
            base_return,
            feedback_clamp_hit,
            session_vol_mult: 1.0,
            regime_vol_mult: 1.0,
            pre_realized_return: 0.0,
            realized_return,
            realized_clamp_hit: false,
            mid_before: mid,
            mid_after: mid * (1.0 + realized_return),
        }
    }

    struct Fixture {
        acc: Measure12aAcc,
    }

    impl Fixture {
        fn new() -> Self {
            prepare_trade_tick(tick());
            Self {
                acc: Measure12aAcc::new(9, OPEN_NS, CLOSE_NS, OFFSET, tick()),
            }
        }

        fn parent(
            &mut self,
            ts: u64,
            level: i64,
            trace: mogwai_data::VolTrace,
            trade_levels: &[i64],
        ) {
            self.acc
                .push_quote(&quote(ts, level), Some(trace))
                .expect("quote");
            for (i, &tl) in trade_levels.iter().enumerate() {
                self.acc
                    .push_trade(&trade(ts + i as u64, tl))
                    .expect("trade");
            }
        }
    }

    /// The Brick G gate test: the accumulator's serialized blocks against
    /// an independent recompute over the same crafted parent stream.
    #[test]
    fn measure12a_matches_independent_recompute() {
        let mut fx = Fixture::new();
        let minute = |m: u64| H23_NS + m * NS_PER_MIN;
        let quiet = vt(0.5, 1.0e-8, 1.0e-8, 1.0e-4, 1.0e-4, 23_000.0, false, false);
        // Minute 0: the extreme - a calm opener, then a 12-sigma parent
        // with two clamp flags printing an 80-tick range.
        fx.parent(minute(0) + 10_000_000_000, 0, quiet, &[0]);
        fx.parent(
            minute(0) + 20_000_000_000,
            40,
            vt(12.0, 9.0e-8, 9.0e-8, 3.0e-4, 3.0e-4, 23_000.0, true, true),
            &[40, -40],
        );
        // Minutes 5..45: forty quiet one-parent minutes (the control
        // candidate pool past the top-32 exclusion).
        for m in 5..45 {
            fx.parent(minute(m) + 30_000_000_000, 0, quiet, &[0]);
        }
        let value = fx.acc.finish(0.0, 0).expect("finish");

        // One complete session.
        let sessions = value["per_session"].as_array().expect("array");
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s["session_date"], "2026-07-07");
        assert_eq!(s["permutations"], serde_json::json!([]));
        assert_eq!(s["segments"][0]["open_ns"].as_u64(), Some(OPEN_NS));

        // Block 1, independently recomputed: 41 populated minutes, one
        // with n=2, quote range 80 half-ticks... The extreme minute's
        // quote-mid moves 40 ticks = 80 half-ticks; trade range 80 ticks.
        let hist = s["block1_hist"].as_array().expect("hist");
        let total_minutes: u64 = hist.iter().map(|r| r["count"].as_u64().unwrap()).sum();
        assert_eq!(total_minutes, 41);
        let extreme_row = hist
            .iter()
            .find(|r| r["n"] == 2)
            .expect("the two-parent extreme minute");
        assert_eq!(extreme_row["trade_range_ticks"], 80);
        assert_eq!(extreme_row["quote_range_half_ticks"], 80);
        assert_eq!(extreme_row["hour"], 23);
        assert_eq!(extreme_row["since_open_bin"], "1800+");
        assert_eq!(extreme_row["until_close_bin"], "1800+");
        let quiet_row = hist.iter().find(|r| r["n"] == 1).expect("quiet minutes");
        assert_eq!(quiet_row["count"], 40);
        assert_eq!(quiet_row["trade_range_ticks"], 0);
        assert_eq!(quiet_row["quote_range_half_ticks"], 0);

        // Block 2 hour 23 w=60, independently recomputed: the hour holds
        // 60 origin-aligned windows minus the hour-crossing last one =
        // 59 scheduled; 42 parents fall in 41 distinct windows (the
        // extreme minute's two parents share one).
        let b2 = &s["block2"]["23"]["60"];
        assert_eq!(b2["scheduled_windows"], 59);
        assert_eq!(b2["zero_windows"], 59 - 41);
        assert_eq!(b2["count_hist"]["1"], 40);
        assert_eq!(b2["count_hist"]["2"], 1);
        assert_eq!(b2["count_p99"], 2);

        // Block 3 hour 23 h=60, independently recomputed with a naive
        // as-of walk over the same (ts, log-mid) series.
        // The canonical convention: ln of the mid in 1e-9 price units
        // (the observed harness logs integer price units).
        let logmid = |level: i64| -> f64 {
            let sum_nanos =
                f64::try_from((px(level - 1) + px(level + 1)) * Decimal::from(1_000_000_000))
                    .unwrap();
            (sum_nanos / 2.0).ln()
        };
        let mids: Vec<(u64, f64)> = {
            let mut v = vec![
                (minute(0) + 10_000_000_000, logmid(0)),
                (minute(0) + 20_000_000_000, logmid(40)),
            ];
            for m in 5..45 {
                v.push((minute(m) + 30_000_000_000, logmid(0)));
            }
            v
        };
        let naive = |h_ns: u64| -> (u64, f64) {
            let mut prev: Option<f64> = None;
            let mut count = 0u64;
            let mut sum_abs = 0.0;
            let mut max_abs = 0.0f64;
            let mut b = OPEN_NS + h_ns;
            while b < OPEN_NS + 80_100_000_000_000 {
                let asof = mids.iter().rev().find(|&&(ts, _)| ts <= b).map(|&(_, m)| m);
                if let Some(cur) = asof {
                    if let Some(p) = prev
                        && (b / NS_PER_HOUR) % 24 == ((b - h_ns) / NS_PER_HOUR) % 24
                        && (b / NS_PER_HOUR) % 24 == 23
                    {
                        let a = (cur - p).abs();
                        count += 1;
                        sum_abs += a;
                        if a > max_abs {
                            max_abs = a;
                        }
                    }
                    prev = Some(cur);
                }
                b += h_ns;
            }
            (count, (sum_abs - max_abs) / (count - 1) as f64)
        };
        let (n60, robust60) = naive(60_000_000_000);
        let b3 = &s["block3"]["cells"]["23"]["60"];
        assert_eq!(b3["return_count"].as_u64(), Some(n60));
        let got = b3["robust_scale"].as_f64().expect("robust");
        assert!((got - robust60).abs() <= 1e-15_f64.max(robust60 * 1e-12));

        // Block 4: 41 adjacent-mid returns, every one below the
        // 1000-return history floor - all warmup, no residuals.
        assert_eq!(s["block4"]["23"]["warmup_excluded"], 41);
        assert_eq!(s["block4"]["23"]["residual_count"], 0);
        assert_eq!(s["block4"]["all"]["warmup_excluded"], 41);
        assert_eq!(s["refusals"], serde_json::json!([]));

        // Forensic: the extreme minute, its trace-grounded fields, and
        // the control chosen by the frozen tie-break.
        let records = value["forensic"]["records"].as_array().expect("records");
        assert_eq!(records.len(), 2);
        let extreme = &records[0];
        assert_eq!(extreme["kind"], "extreme_range");
        assert_eq!(extreme["minute_start_ns"].as_u64(), Some(minute(0)));
        assert_eq!(extreme["parent_count"], 2);
        assert_eq!(extreme["trade_count"], 3);
        assert_eq!(extreme["traced_parents"], 2);
        assert_eq!(extreme["largest_innovation_std"], 12.0);
        assert_eq!(extreme["innovation_exceed_8"], 1);
        assert_eq!(extreme["innovation_exceed_16"], 0);
        // The 12-sigma parent lifts the running quote range to its final
        // value, so initiation holds.
        assert_eq!(extreme["initiation"], true);
        // Two simultaneous clamp flags on one parent contribute two.
        assert_eq!(extreme["clamp_hits"], 2);
        assert_eq!(extreme["max_signed_run"], 2);
        // The first measured parent opens this minute: sigma_start and
        // sigma_escalation refused with one owning record.
        assert!(extreme["sigma_start"].is_null());
        assert!(extreme["sigma_escalation"].is_null());
        // arch_share_next: the 12-sigma parent's successor is the first
        // quiet minute-5 parent.
        let expected_share = ARCH_12A * 3.0e-4_f64.powi(2) / 1.0e-8;
        let got_share = extreme["arch_share_next"].as_f64().expect("share");
        assert!((got_share - expected_share).abs() < 1e-12);
        let refusals = value["forensic"]["refusals"].as_array().expect("refusals");
        assert_eq!(refusals.len(), 1);
        assert!(
            refusals[0]["cell"]
                .as_str()
                .unwrap()
                .contains("sigma_start"),
            "{refusals:?}"
        );
        // Control: same segment-hour, at or below the median range (0),
        // outside the top-32 by range; the tie among the nine eligible
        // quiet minutes resolves by the frozen tuple_mix rank.
        let control = &records[1];
        assert_eq!(control["kind"], "control");
        assert_eq!(
            control["matched_extreme_minute_start"].as_u64(),
            Some(minute(0))
        );
        let eligible: Vec<u64> = (36..45).map(minute).collect();
        let expected_control = eligible
            .iter()
            .copied()
            .min_by_key(|&m| (tuple_mix(CONTROL_TIE_BASE_SEED, &[9, minute(0), m]), m))
            .unwrap();
        assert_eq!(control["minute_start_ns"].as_u64(), Some(expected_control));
    }

    #[test]
    fn initiation_survives_a_minute_straddling_burst() {
        // A burst whose later children cross the minute boundary must
        // not resolve the minute's initiation before its own parent is
        // attributed back to the first-child minute: minute closure is
        // parent-driven, not trade-driven.
        let mut fx = Fixture::new();
        let minute = |m: u64| H23_NS + m * NS_PER_MIN;
        let quiet = vt(0.5, 1.0e-8, 1.0e-8, 1.0e-4, 1.0e-4, 23_000.0, false, false);
        fx.parent(minute(0) + 10_000_000_000, 0, quiet, &[0]);
        // The 12-sigma parent's first child sits at +58 s (minute 0);
        // its second child lands in minute 1 BEFORE the parent closes.
        fx.acc
            .push_quote(
                &quote(minute(0) + 58_000_000_000, 40),
                Some(vt(
                    12.0, 9.0e-8, 9.0e-8, 3.0e-4, 3.0e-4, 23_000.0, false, false,
                )),
            )
            .expect("quote");
        fx.acc
            .push_trade(&trade(minute(0) + 58_000_000_000, 40))
            .expect("trade");
        fx.acc
            .push_trade(&trade(minute(1) + 2_000_000_000, 40))
            .expect("straddling trade");
        for m in 5..45 {
            fx.parent(minute(m) + 30_000_000_000, 0, quiet, &[0]);
        }
        let value = fx.acc.finish(0.0, 0).expect("finish");
        let extreme = &value["forensic"]["records"][0];
        assert_eq!(extreme["kind"], "extreme_range");
        assert_eq!(extreme["minute_start_ns"].as_u64(), Some(minute(0)));
        assert_eq!(
            extreme["initiation"], true,
            "the straddling burst's own breakpoint must decide initiation"
        );
    }

    #[test]
    fn a_shared_control_refuses_once_and_a_new_largest_clears_the_share() {
        // Both extremes select the SAME sole eligible control: the
        // control emits one record per extreme, but a refused cell on it
        // carries exactly ONE logical RefusalRec. And a parent that
        // becomes the minute's largest innovation AFTER a share was
        // resolved for the previous largest must null the stale share.
        let mut fx = Fixture::new();
        let minute = |m: u64| H23_NS + m * NS_PER_MIN;
        let quiet = vt(0.5, 1.0e-8, 1.0e-8, 1.0e-4, 1.0e-4, 23_000.0, false, false);
        // 29 quiet minutes (0..28), all range 0.
        for m in 0..29 {
            fx.parent(minute(m) + 30_000_000_000, 0, quiet, &[0]);
        }
        // The range extreme (minute 31): four parents, range 100 ->
        // range/sqrt(4) = 50. Its first big parent (inn 9) gains a
        // resolved share from its successor, then the LAST parent of the
        // walk-wide stream (inn 12, below) takes over as largest.
        fx.parent(
            minute(31) + 10_000_000_000,
            0,
            vt(9.0, 9.0e-8, 9.0e-8, 3.0e-4, 3.0e-4, 23_000.0, false, false),
            &[50, -50],
        );
        for k in 0..3 {
            fx.parent(
                minute(31) + 20_000_000_000 + k * 5_000_000_000,
                0,
                quiet,
                &[0],
            );
        }
        // The sqrt extreme (minute 32): one parent, range 90 -> 90.
        fx.parent(minute(32) + 10_000_000_000, 0, quiet, &[45, -45]);
        // Minute 33: range 1 (above the median 0, so never a control)
        // whose parent realizes sigma2 = 0 - the control's predecessor.
        fx.parent(
            minute(33) + 10_000_000_000,
            0,
            vt(0.5, 1.0e-8, 0.0, 0.0, 0.0, 23_000.0, false, false),
            &[0, 1],
        );
        // Minute 36: the sole eligible control (range 0, past the
        // top-32), with a nonpositive sigma_start from the zero-sigma
        // predecessor.
        fx.parent(minute(36) + 10_000_000_000, 0, quiet, &[0]);
        let value = fx.acc.finish(0.0, 0).expect("finish");
        let records = value["forensic"]["records"].as_array().expect("records");
        assert_eq!(records.len(), 4, "{records:?}");
        assert_eq!(records[0]["kind"], "extreme_range");
        assert_eq!(records[0]["minute_start_ns"].as_u64(), Some(minute(31)));
        assert_eq!(records[1]["kind"], "control");
        assert_eq!(
            records[1]["matched_extreme_minute_start"].as_u64(),
            Some(minute(31))
        );
        assert_eq!(records[1]["minute_start_ns"].as_u64(), Some(minute(36)));
        assert_eq!(records[2]["kind"], "extreme_sqrt");
        assert_eq!(records[2]["minute_start_ns"].as_u64(), Some(minute(32)));
        assert_eq!(records[3]["kind"], "control");
        assert_eq!(
            records[3]["matched_extreme_minute_start"].as_u64(),
            Some(minute(32))
        );
        assert_eq!(records[3]["minute_start_ns"].as_u64(), Some(minute(36)));
        // Both control records refuse the escalation; exactly ONE
        // logical refusal record exists for that cell.
        assert!(records[1]["sigma_escalation"].is_null());
        assert!(records[3]["sigma_escalation"].is_null());
        let refusals = value["forensic"]["refusals"].as_array().expect("refusals");
        let escalations: Vec<_> = refusals
            .iter()
            .filter(|r| r["cell"].as_str().unwrap().contains("sigma_escalation"))
            .collect();
        assert_eq!(escalations.len(), 1, "{refusals:?}");
    }

    #[test]
    fn a_new_largest_innovation_nulls_the_stale_share() {
        // Within one minute: parent A (inn 9) resolves a share when
        // parent B arrives; B (inn 12) becomes the largest and never
        // gains a successor, so arch_share_next must be null while the
        // minute max keeps A's resolved share.
        let mut fx = Fixture::new();
        let minute = |m: u64| H23_NS + m * NS_PER_MIN;
        let big = |inn: f64| vt(inn, 9.0e-8, 9.0e-8, 3.0e-4, 3.0e-4, 23_000.0, false, false);
        fx.parent(minute(0) + 10_000_000_000, 40, big(9.0), &[40, -40]);
        fx.parent(minute(0) + 20_000_000_000, 0, big(12.0), &[0]);
        let value = fx.acc.finish(0.0, 0).expect("finish");
        let extreme = &value["forensic"]["records"][0];
        assert_eq!(extreme["largest_innovation_std"], 12.0);
        assert!(
            extreme["arch_share_next"].is_null(),
            "the stale share of the superseded largest leaked: {extreme}"
        );
        assert!(extreme["arch_share_minute_max"].as_f64().is_some());
    }

    #[test]
    fn block4_omits_a_nonpositive_scale_and_records() {
        // A flat mid drives the trailing one-max-trimmed scale to zero
        // once the 1000-return history fills: the residuals are OMITTED
        // (Amendment F) and recorded, never a hard error.
        let mut fx = Fixture::new();
        let quiet = vt(0.0, 1.0e-8, 1.0e-8, 0.0, 0.0, 23_000.0, false, false);
        for k in 0..1500u64 {
            fx.parent(H23_NS + k * 200_000_000, 0, quiet, &[0]);
        }
        let value = fx.acc.finish(0.0, 0).expect("finish");
        let s = &value["per_session"][0];
        assert_eq!(s["block4"]["23"]["residual_count"], 0);
        assert!(s["block4"]["23"]["warmup_excluded"].as_u64().unwrap() >= 1000);
        let refusals = s["refusals"].as_array().expect("refusals");
        assert_eq!(refusals.len(), 1);
        assert!(
            refusals[0]["cell"]
                .as_str()
                .unwrap()
                .contains("standardizer")
        );
        assert!(refusals[0]["scope"].as_str().unwrap().contains("seed 9"));
    }
}
