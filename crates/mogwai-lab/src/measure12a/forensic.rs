// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Block 5, the trace-grounded forensic record set (spec 3.4b). Generated
//! side only: every field here is grounded in a `VolTrace`, which the
//! observed corpus has no analogue of.
//!
//! The conventions that had to be pinned, each with a test vector in
//! `super::tests`:
//!
//! - minute closure is parent-driven, never trade-driven: a burst's later
//!   children can cross the minute boundary before their parent finalizes,
//!   and closing on the trade would resolve initiation without that parent's
//!   quote extrema and breakpoint;
//! - a superseding largest innovation nulls the stale `arch_share_next` -
//!   the deferred share described the previous largest parent, not this one;
//! - a control shared by two extremes refuses exactly once per logical cell,
//!   even though the record is emitted once per extreme;
//! - the frozen tie-breaks: the earlier minute wins a range tie, and the
//!   control tie resolves on `tuple_mix(CONTROL_TIE_BASE_SEED, [seed,
//!   extreme minute start, candidate minute start])`, then on the earlier
//!   minute.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{LabError, LabResult};
use crate::kernel::tuple_mix;
use crate::measure12a::{NS_PER_HOUR, NS_PER_MIN, exact_div};
use crate::subcontract::{
    CONTROL_TIE_BASE_SEED, INNOVATION_EXCEED_ABS, TICK_UNITS, TOP_MINUTE_RECORDS,
};

/// Per-minute forensic state.
#[derive(Debug)]
pub(crate) struct MinuteRec {
    pub(crate) segment_index: u8,
    pub(crate) parent_count: u64,
    pub(crate) trade_count: u64,
    pub(crate) traced: u64,
    pub(crate) largest_inn: f64,
    pub(crate) largest_inn_ts: u64,
    /// Walk-global sequence number of the largest-innovation parent, so the
    /// deferred `arch_share_next` can find its successor.
    pub(crate) largest_inn_seq: u64,
    pub(crate) exceed: [u64; 3],
    pub(crate) sigma_start: Option<f64>,
    pub(crate) sigma_peak: f64,
    pub(crate) sigma_end: f64,
    pub(crate) latent_lo: f64,
    pub(crate) latent_hi: f64,
    pub(crate) max_signed_run: u64,
    pub(crate) cur_run: u64,
    pub(crate) cur_sign: i8,
    pub(crate) clamp_hits: u64,
    pub(crate) arch_share_next: Option<f64>,
    pub(crate) arch_share_max: Option<f64>,
    /// `(parent ts, running quote-mid range in half ticks)` breakpoints,
    /// retained only while the minute is open; initiation resolves at minute
    /// close and the vector is dropped.
    pub(crate) breakpoints: Vec<(u64, i64)>,
    pub(crate) quote_lo: Option<i64>,
    pub(crate) quote_hi: Option<i64>,
    pub(crate) trade_lo: Option<i64>,
    pub(crate) trade_hi: Option<i64>,
    pub(crate) initiation: bool,
}

impl MinuteRec {
    pub(crate) fn new(segment_index: u8) -> Self {
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

    fn trade_ticks(&self) -> LabResult<i64> {
        match (self.trade_lo, self.trade_hi) {
            (Some(lo), Some(hi)) => exact_div(hi - lo, TICK_UNITS, "trade range ticks"),
            _ => Ok(0),
        }
    }
}

/// The 3.4b selection: the two extremes, their controls, and the refusals.
pub(crate) fn select(
    minutes: &BTreeMap<u64, MinuteRec>,
    seed: u64,
    tick_f: f64,
) -> LabResult<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
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
    // Extreme by trade range, earlier minute on ties (matching
    // rank_top_minutes).
    let mut ranked: Vec<(i64, u64)> = Vec::new();
    for &(m, r) in &populated {
        ranked.push((r.trade_ticks()?, m));
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
        #[expect(
            clippy::cast_precision_loss,
            reason = "tick ranges and counts stay small"
        )]
        let v = ticks as f64 / (n as f64).sqrt();
        best_sqrt = match best_sqrt {
            Some((bv, bm)) if bv > v || (bv == v && bm < m) => Some((bv, bm)),
            _ => Some((v, m)),
        };
    }
    // Deduplicate: a shared minute emits once, as extreme_range.
    let mut extremes: Vec<(u64, &'static str)> = vec![(extreme_range_minute, "extreme_range")];
    if let Some((_, m)) = best_sqrt
        && m != extreme_range_minute
    {
        extremes.push((m, "extreme_sqrt"));
    }
    let top_exclude: BTreeSet<u64> = ranked
        .iter()
        .take(TOP_MINUTE_RECORDS as usize)
        .map(|&(_, m)| m)
        .collect();
    let extreme_set: BTreeSet<u64> = extremes.iter().map(|&(m, _)| m).collect();

    // Per (segment, hour) trade-range medians over populated minutes, on the
    // nearest-rank (ceil n/2) convention.
    let mut group_ranges: BTreeMap<(u8, u64), Vec<i64>> = BTreeMap::new();
    for &(m, r) in &populated {
        let hour = (m * NS_PER_MIN / NS_PER_HOUR) % 24;
        group_ranges
            .entry((r.segment_index, hour))
            .or_default()
            .push(r.trade_ticks()?);
    }
    for v in group_ranges.values_mut() {
        v.sort_unstable();
    }
    let median_of = |key: (u8, u64)| -> Option<i64> {
        let v = group_ranges.get(&key)?;
        Some(v[v.len().div_ceil(2) - 1])
    };

    for &(minute, kind) in &extremes {
        let rec = &minutes[&minute];
        // Fail closed rather than fabricate: every selected minute must carry
        // at least one traced parent (the schema has no empty-set convention
        // for the trace-grounded fields).
        if rec.traced == 0 {
            return Err(LabError::refusal(format!(
                "selected forensic minute {} has no traced parent; the schema has no \
                 empty-set convention - stop for an amendment",
                minute * NS_PER_MIN
            )));
        }
        records.push(record(
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
        let mut best: Option<(f64, u64, u64)> = None; // (distance, rank, minute)
        for &(m, r) in &populated {
            if m == minute || extreme_set.contains(&m) || top_exclude.contains(&m) {
                continue;
            }
            let m_hour = (m * NS_PER_MIN / NS_PER_HOUR) % 24;
            if r.segment_index != rec.segment_index || m_hour != hour {
                continue;
            }
            let Some(med) = median else { continue };
            if r.trade_ticks()? > med {
                continue;
            }
            #[expect(clippy::cast_precision_loss, reason = "parent counts stay small")]
            let dist = ((r.parent_count as f64).ln_1p() - (n_e as f64).ln_1p()).abs();
            let rank = tuple_mix(
                CONTROL_TIE_BASE_SEED as u64,
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
                    return Err(LabError::refusal(format!(
                        "selected control minute {} has no traced parent; stop for an amendment",
                        control_minute * NS_PER_MIN
                    )));
                }
                records.push(record(
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

fn record(
    seed: u64,
    kind: &str,
    minute: u64,
    rec: &MinuteRec,
    matched: Option<u64>,
    tick_f: f64,
    refusals: &mut Vec<serde_json::Value>,
) -> LabResult<serde_json::Value> {
    let minute_start = minute * NS_PER_MIN;
    let trade_ticks = rec.trade_ticks()?;
    let quote_half = match (rec.quote_lo, rec.quote_hi) {
        (Some(lo), Some(hi)) => Some(hi - lo),
        _ => None,
    };
    let latent_ticks = if rec.latent_hi >= rec.latent_lo {
        (rec.latent_hi - rec.latent_lo) / tick_f
    } else {
        0.0
    };
    // Exactly one logical refusal per refused cell, even when the same minute
    // serves as the control for both extremes and this function runs twice.
    let mut refuse_once = |cell: String, reason: &str| {
        let entry = serde_json::json!({
            "scope": format!("seed {seed} forensic"),
            "cell": cell,
            "reason": reason,
        });
        if !refusals.contains(&entry) {
            refusals.push(entry);
        }
    };
    // sigma_start: null with one refusal owning both it and the dependent
    // sigma_escalation when the minute opens the measured walk.
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
    // Ratio nulls from an absent or zero denominator are defined emptiness,
    // never refusals.
    #[expect(clippy::cast_precision_loss, reason = "tick ranges stay small")]
    let trade_to_quote = quote_half
        .filter(|&q| q > 0)
        .map(|q| trade_ticks as f64 / (q as f64 / 2.0));
    #[expect(clippy::cast_precision_loss, reason = "tick ranges stay small")]
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

/// The three innovation-exceedance bounds as `f64`, in the frozen order.
pub(crate) fn innovation_bounds() -> [f64; 3] {
    let mut out = [0.0f64; 3];
    for (slot, &bound) in out.iter_mut().zip(INNOVATION_EXCEED_ABS) {
        #[expect(clippy::cast_precision_loss, reason = "4, 8, 16")]
        let b = bound as f64;
        *slot = b;
    }
    out
}
