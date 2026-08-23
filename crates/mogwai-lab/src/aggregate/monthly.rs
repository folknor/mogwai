// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The monthly aggregates (spec 3.5): pooling the per-session block records
//! into the descriptive month, plus the 8-seed central blocks.
//!
//! Ported from `analysis/mnq_fit.py`'s `pool_block1_hists`,
//! `hist_to_records`, `block1_summary`, `block1_blocks`, `pool_block2`,
//! `finish_block2_cell`, `aggregate_block3`, `aggregate_block4`,
//! `aggregate_permutations`, `tree_median` and `central_blocks_from_seeds`.
//!
//! Two conventions are worth stating once, because both are load-bearing and
//! neither is obvious:
//!
//! - **The monthly block-3/4 aggregates skip non-qualifying sessions
//!   silently.** This is the descriptive record; the ladder's Q1 all-session
//!   qualification is a separate, stricter rule enforced in
//!   [`super::family`]. A cell can therefore carry a monthly median while
//!   every metric over it is refused.
//! - **`aggregate_block4` sorts its hours as strings**, so the emitted key
//!   order is `"0", "1", "10", ..., "all"` - the pooled `"all"` cell sorts
//!   last by accident of the alphabet, not by design.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;

use super::{ja, jf_opt, ji, jint, jnum, js};
use crate::error::{LabError, LabResult};
use crate::kernel::{median_or_none, weighted_nearest_rank, weighted_nearest_rank_f64};
use crate::session::parent_count_bin;
use crate::subcontract::{
    EXCEEDANCE_TICKS, MIN_1S_CELL_RETURNS, MIN_5S_CELL_RETURNS, MIN_15S_CELL_RETURNS,
    MIN_60S_CELL_RETURNS, MIN_300S_CELL_RETURNS, MIN_BOUNDARY_60S_CELL_RETURNS, MIN_RESIDUAL_CELL,
    PARENT_COUNT_BIN_NAMES, PERMUTATION_REPLICATES, PERMUTATION_VARIANTS, SINCE_OPEN_BIN_NAMES,
    UNTIL_CLOSE_BIN_NAMES, WALL_HORIZONS_S,
};

/// One pooled block-1 histogram key: `(n, quote half-ticks, trade ticks,
/// hour, since-open bin, until-close bin)`. The quote range carries `-1` for
/// the Python `None` (no valid-book parent in the minute), which is exactly
/// the sort key `hist_to_records` uses - so the natural `BTreeMap` order is
/// the frozen record order, with the null quote sorting first.
pub type PooledKey = (i64, i64, i64, i64, &'static str, &'static str);

/// The pooled sparse joint histogram: key -> minute count, in first-insertion
/// order.
///
/// The order is not cosmetic. The 5.2 count substitution accumulates its
/// weighted totals (`total_w`, `exceed_w` and the weighted cumulative counts
/// of [`super::countsub::CountSubEval`]) by walking this map, and those are
/// float sums: reordering them moves the last ulp of
/// `counterfactual_exceed_968` and can move a binary search across a support
/// boundary. The Python's `dict` is insertion-ordered, so this port is too.
/// Everywhere a sorted walk is what the Python asks for - the record order,
/// the hour and label key sets - the call site sorts explicitly.
#[derive(Debug, Default, Clone)]
pub struct PooledHist {
    keys: Vec<PooledKey>,
    counts: Vec<i64>,
    index: HashMap<PooledKey, usize>,
}

impl PooledHist {
    fn add(&mut self, key: PooledKey, count: i64) {
        match self.index.get(&key) {
            Some(&i) => self.counts[i] += count,
            None => {
                self.index.insert(key, self.keys.len());
                self.keys.push(key);
                self.counts.push(count);
            }
        }
    }

    /// The entries in first-insertion order - the Python `dict.items()`.
    pub fn iter(&self) -> impl Iterator<Item = (&PooledKey, i64)> {
        self.keys.iter().zip(self.counts.iter().copied())
    }

    /// The entries in ascending key order - the Python `sorted(...)`.
    #[must_use]
    pub fn sorted(&self) -> Vec<(PooledKey, i64)> {
        let mut rows: Vec<(PooledKey, i64)> = self.iter().map(|(k, c)| (*k, c)).collect();
        rows.sort_unstable();
        rows
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }
}

fn intern(name: &str, table: &[&'static str], what: &str) -> LabResult<&'static str> {
    table
        .iter()
        .copied()
        .find(|n| *n == name)
        .ok_or_else(|| LabError::refusal(format!("unknown {what} label {name:?}")))
}

/// `horizon_floor`: the minimum return/window count a horizon's cell needs
/// before a session may vote.
#[must_use]
pub fn horizon_floor(h: i64) -> i64 {
    match h {
        1 => MIN_1S_CELL_RETURNS,
        5 => MIN_5S_CELL_RETURNS,
        15 => MIN_15S_CELL_RETURNS,
        60 => MIN_60S_CELL_RETURNS,
        300 => MIN_300S_CELL_RETURNS,
        other => panic!("no floor is defined for horizon {other}"),
    }
}

/// `_robust_from_stats`: the one-max-trimmed mean absolute scale over a
/// cell's sufficient statistics, refused below two observations.
#[must_use]
pub fn robust_from_stats(count: i64, sum_abs: f64, max_abs: f64) -> Option<f64> {
    if count < 2 {
        return None;
    }
    #[expect(clippy::cast_precision_loss, reason = "counts stay far below 2^53")]
    let denom = (count - 1) as f64;
    Some((sum_abs - max_abs) / denom)
}

// -- Block 1 ----------------------------------------------------------------

/// `pool_block1_hists`: sum the per-session sparse histograms key-wise.
pub fn pool_block1_hists(hists: &[&[Value]]) -> LabResult<PooledHist> {
    let mut pooled = PooledHist::default();
    for hist in hists {
        for rec in *hist {
            let key = (
                ji(rec, "n"),
                super::ji_opt(rec, "quote_range_half_ticks").unwrap_or(-1),
                ji(rec, "trade_range_ticks"),
                ji(rec, "hour"),
                intern(
                    js(rec, "since_open_bin"),
                    SINCE_OPEN_BIN_NAMES,
                    "since-open",
                )?,
                intern(
                    js(rec, "until_close_bin"),
                    UNTIL_CLOSE_BIN_NAMES,
                    "until-close",
                )?,
            );
            pooled.add(key, ji(rec, "count"));
        }
    }
    Ok(pooled)
}

/// `pool_block1_hists` over whole per-session record arrays.
pub fn pool_session_hists(per_session: &[Value]) -> LabResult<PooledHist> {
    let hists: Vec<&[Value]> = per_session.iter().map(|r| ja(r, "block1_hist")).collect();
    pool_block1_hists(&hists)
}

/// `hist_to_records`: the pooled histogram back to the frozen record order.
#[must_use]
pub fn hist_to_records(pooled: &PooledHist) -> Value {
    Value::Array(
        pooled
            .sorted()
            .into_iter()
            .map(|((n, q, t, h, since, until), count)| {
                serde_json::json!({
                    "n": n,
                    "quote_range_half_ticks": if q < 0 { Value::Null } else { serde_json::json!(q) },
                    "trade_range_ticks": t,
                    "hour": h,
                    "since_open_bin": since,
                    "until_close_bin": until,
                    "count": count,
                })
            })
            .collect(),
    )
}

/// `block1_summary`: one summary over the pooled histogram, optionally
/// restricted to one hour and/or one `(since, until)` label pair.
#[must_use]
pub fn block1_summary(
    pooled: &PooledHist,
    hour_filter: Option<i64>,
    label_filter: Option<(&str, &str)>,
) -> Value {
    let rows: Vec<(&PooledKey, i64)> = pooled
        .iter()
        .filter(|(k, _)| hour_filter.is_none_or(|h| k.3 == h))
        .filter(|(k, _)| label_filter.is_none_or(|(s, u)| k.4 == s && k.5 == u))
        .collect();
    let minute_count: i64 = rows.iter().map(|(_, c)| *c).sum();
    let quote_rows: Vec<(&PooledKey, i64)> =
        rows.iter().filter(|(k, _)| k.1 >= 0).copied().collect();
    let quote_denom: i64 = quote_rows.iter().map(|(_, c)| *c).sum();

    let n_pairs: Vec<(i64, i64)> = rows.iter().map(|(k, c)| (k.0, *c)).collect();
    let tr_pairs: Vec<(i64, i64)> = rows.iter().map(|(k, c)| (k.2, *c)).collect();
    let qr_pairs: Vec<(i64, i64)> = quote_rows.iter().map(|(k, c)| (k.1, *c)).collect();
    let sq_pairs: Vec<(f64, i64)> = rows
        .iter()
        .filter(|(k, _)| k.0 >= 1)
        .map(|(k, c)| (sqrt_n_value(k.2, k.0), *c))
        .collect();
    let exceed: Vec<i64> = EXCEEDANCE_TICKS
        .iter()
        .map(|&t| rows.iter().filter(|(k, _)| k.2 > t).map(|(_, c)| *c).sum())
        .collect();
    let tr_p99 = weighted_nearest_rank(&tr_pairs, 0.99);
    let qr_p99 = weighted_nearest_rank(&qr_pairs, 0.99);
    // The half-ticks become ticks before the division, never after.
    let ratio = match (tr_p99, qr_p99) {
        (Some(t), Some(q)) if q != 0 => {
            #[expect(clippy::cast_precision_loss, reason = "tick ranges stay small")]
            let v = t as f64 / (q as f64 / 2.0);
            Some(v)
        }
        _ => None,
    };

    let by_bin: serde_json::Map<String, Value> = PARENT_COUNT_BIN_NAMES
        .iter()
        .map(|&name| ((*name).to_string(), bin_summary(&rows, name)))
        .collect();

    serde_json::json!({
        "minute_count": minute_count,
        "quote_range_denominator": quote_denom,
        "n_p50": jint(weighted_nearest_rank(&n_pairs, 0.50)),
        "n_p90": jint(weighted_nearest_rank(&n_pairs, 0.90)),
        "n_p99": jint(weighted_nearest_rank(&n_pairs, 0.99)),
        "n_p999": jint(weighted_nearest_rank(&n_pairs, 0.999)),
        "quote_range_p50": jint(weighted_nearest_rank(&qr_pairs, 0.50)),
        "quote_range_p90": jint(weighted_nearest_rank(&qr_pairs, 0.90)),
        "quote_range_p99": jint(qr_p99),
        "quote_range_p999": jint(weighted_nearest_rank(&qr_pairs, 0.999)),
        "trade_range_p50": jint(weighted_nearest_rank(&tr_pairs, 0.50)),
        "trade_range_p90": jint(weighted_nearest_rank(&tr_pairs, 0.90)),
        "trade_range_p99": jint(tr_p99),
        "trade_range_p999": jint(weighted_nearest_rank(&tr_pairs, 0.999)),
        "trade_range_sqrt_n_p50": jnum(weighted_nearest_rank_f64(&sq_pairs, 0.50)),
        "trade_range_sqrt_n_p90": jnum(weighted_nearest_rank_f64(&sq_pairs, 0.90)),
        "trade_range_sqrt_n_p99": jnum(weighted_nearest_rank_f64(&sq_pairs, 0.99)),
        "exceed_399": exceed[0],
        "exceed_642": exceed[1],
        "exceed_968": exceed[2],
        "denominator": minute_count,
        "trade_to_quote_p99_ratio": jnum(ratio),
        "by_parent_count_bin": Value::Object(by_bin),
    })
}

/// `trade_range_ticks / sqrt(n)` - the conditional shape statistic. `n` is
/// at least one wherever this is called (bin `"0"` is excluded by name).
#[must_use]
pub(crate) fn sqrt_n_value(trade_ticks: i64, n: i64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "tick ranges and counts stay small"
    )]
    let v = trade_ticks as f64 / (n as f64).sqrt();
    v
}

fn bin_summary(rows: &[(&PooledKey, i64)], bin_name: &str) -> Value {
    let brows: Vec<(&PooledKey, i64)> = rows
        .iter()
        .filter(|(k, _)| parent_count_bin(k.0) == bin_name)
        .copied()
        .collect();
    let bcount: i64 = brows.iter().map(|(_, c)| *c).sum();
    let bq: Vec<(i64, i64)> = brows
        .iter()
        .filter(|(k, _)| k.1 >= 0)
        .map(|(k, c)| (k.1, *c))
        .collect();
    let bt: Vec<(i64, i64)> = brows.iter().map(|(k, c)| (k.2, *c)).collect();
    // Bin "0" carries no sqrt(N) support: the statistic is undefined at
    // n = 0, so the Python builds an empty pair list rather than filtering.
    let bs: Vec<(f64, i64)> = if bin_name == "0" {
        Vec::new()
    } else {
        brows
            .iter()
            .map(|(k, c)| (sqrt_n_value(k.2, k.0), *c))
            .collect()
    };
    serde_json::json!({
        "minute_count": bcount,
        "quote_range_denominator": bq.iter().map(|(_, c)| *c).sum::<i64>(),
        "quote_range_p50": jint(weighted_nearest_rank(&bq, 0.50)),
        "quote_range_p90": jint(weighted_nearest_rank(&bq, 0.90)),
        "quote_range_p99": jint(weighted_nearest_rank(&bq, 0.99)),
        "quote_range_p999": jint(weighted_nearest_rank(&bq, 0.999)),
        "trade_range_p50": jint(weighted_nearest_rank(&bt, 0.50)),
        "trade_range_p90": jint(weighted_nearest_rank(&bt, 0.90)),
        "trade_range_p99": jint(weighted_nearest_rank(&bt, 0.99)),
        "trade_range_p999": jint(weighted_nearest_rank(&bt, 0.999)),
        "trade_range_sqrt_n_p50": jnum(weighted_nearest_rank_f64(&bs, 0.50)),
        "trade_range_sqrt_n_p90": jnum(weighted_nearest_rank_f64(&bs, 0.90)),
        "trade_range_sqrt_n_p99": jnum(weighted_nearest_rank_f64(&bs, 0.99)),
    })
}

/// `block1_blocks`: the histogram, the per-hour summaries and the per-label
/// per-hour summaries.
#[must_use]
pub fn block1_blocks(pooled: &PooledHist) -> Value {
    let hours: BTreeSet<i64> = pooled.iter().map(|(k, _)| k.3).collect();
    let label_pairs: BTreeSet<(&'static str, &'static str)> =
        pooled.iter().map(|(k, _)| (k.4, k.5)).collect();
    let summary: serde_json::Map<String, Value> = hours
        .iter()
        .map(|&h| (h.to_string(), block1_summary(pooled, Some(h), None)))
        .collect();
    let by_labels: serde_json::Map<String, Value> = label_pairs
        .iter()
        .map(|&(s, u)| {
            let inner: serde_json::Map<String, Value> = pooled
                .iter()
                .filter(|(k, _)| k.4 == s && k.5 == u)
                .map(|(k, _)| k.3)
                .collect::<BTreeSet<i64>>()
                .iter()
                .map(|&h| (h.to_string(), block1_summary(pooled, Some(h), Some((s, u)))))
                .collect();
            (format!("{s}|{u}"), Value::Object(inner))
        })
        .collect();
    serde_json::json!({
        "hist": hist_to_records(pooled),
        "summary": Value::Object(summary),
        "by_labels": Value::Object(by_labels),
    })
}

// -- Block 2 ----------------------------------------------------------------

#[derive(Default, Debug)]
struct Block2Pool {
    scheduled: i64,
    zeros: i64,
    count_hist: BTreeMap<i64, i64>,
    run_hist: BTreeMap<i64, i64>,
    paired: i64,
    sum_x: i64,
    sum_y: i64,
    sumsq_x: i64,
    sumsq_y: i64,
    sum_xy: i64,
}

/// `pool_block2`: pool the exact per-session histograms and lag-1 moments,
/// then re-derive the scalars from the pooled sufficient statistics (never a
/// mean of per-session scalars).
#[must_use]
pub fn pool_block2(sessions: &[&Value]) -> Value {
    let mut pooled: BTreeMap<(i64, i64), Block2Pool> = BTreeMap::new();
    for rec in sessions {
        let obj = rec.as_object().expect("block2 is an object");
        for (hour_s, per_w) in obj {
            for (w_s, c) in per_w.as_object().expect("a window map") {
                let key = (
                    hour_s.parse().expect("an hour key"),
                    w_s.parse().expect("a window key"),
                );
                let p = pooled.entry(key).or_default();
                p.scheduled += ji(c, "scheduled_windows");
                p.zeros += ji(c, "zero_windows");
                for (k, v) in c["count_hist"].as_object().expect("count_hist") {
                    *p.count_hist
                        .entry(k.parse().expect("a count key"))
                        .or_insert(0) += v.as_i64().expect("a count");
                }
                for (k, v) in c["run_length_hist"].as_object().expect("run_length_hist") {
                    *p.run_hist.entry(k.parse().expect("a run key")).or_insert(0) +=
                        v.as_i64().expect("a count");
                }
                p.paired += ji(c, "paired_lag_count");
                p.sum_x += ji(c, "sum_x");
                p.sum_y += ji(c, "sum_y");
                p.sumsq_x += ji(c, "sumsq_x");
                p.sumsq_y += ji(c, "sumsq_y");
                p.sum_xy += ji(c, "sum_xy");
            }
        }
    }
    let mut out = serde_json::Map::new();
    for (&(hour, w), p) in &pooled {
        out.entry(hour.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .expect("hour entry is an object")
            .insert(w.to_string(), finish_block2_cell(p));
    }
    Value::Object(out)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "every count stays far below 2^53"
)]
fn finish_block2_cell(p: &Block2Pool) -> Value {
    let total: i64 = p.count_hist.values().sum();
    let ssum: i64 = p.count_hist.iter().map(|(&k, &v)| k * v).sum();
    let ssq: i64 = p.count_hist.iter().map(|(&k, &v)| k * k * v).sum();
    let mean = (total != 0).then(|| ssum as f64 / total as f64);
    let var = mean.map(|m| ssq as f64 / total as f64 - m * m);
    // `fano = var / mean if mean else None`: a zero mean is falsey in the
    // Python, so an all-zero cell refuses rather than dividing.
    let fano = match (mean, var) {
        (Some(m), Some(v)) if m != 0.0 => Some(v / m),
        _ => None,
    };
    let n = p.paired;
    let lag1 = if n >= 2 {
        let nf = n as f64;
        let vx = p.sumsq_x as f64 - (p.sum_x as f64).powi(2) / nf;
        let vy = p.sumsq_y as f64 - (p.sum_y as f64).powi(2) / nf;
        if vx > 0.0 && vy > 0.0 {
            Some((p.sum_xy as f64 - p.sum_x as f64 * p.sum_y as f64 / nf) / (vx * vy).sqrt())
        } else {
            None
        }
    } else {
        None
    };
    let count_pairs: Vec<(i64, i64)> = p.count_hist.iter().map(|(&k, &v)| (k, v)).collect();
    let run_pairs: Vec<(i64, i64)> = p.run_hist.iter().map(|(&k, &v)| (k, v)).collect();
    serde_json::json!({
        "scheduled_windows": p.scheduled,
        "zero_windows": p.zeros,
        "count_hist": p.count_hist.iter()
            .map(|(&k, &v)| (k.to_string(), serde_json::json!(v)))
            .collect::<serde_json::Map<_, _>>(),
        "run_length_hist": p.run_hist.iter()
            .map(|(&k, &v)| (k.to_string(), serde_json::json!(v)))
            .collect::<serde_json::Map<_, _>>(),
        "paired_lag_count": n,
        "sum_x": p.sum_x, "sum_y": p.sum_y,
        "sumsq_x": p.sumsq_x, "sumsq_y": p.sumsq_y,
        "sum_xy": p.sum_xy,
        "zero_fraction": if p.scheduled != 0 {
            serde_json::json!(p.zeros as f64 / p.scheduled as f64)
        } else {
            Value::Null
        },
        "mean": jnum(mean),
        "fano": jnum(fano),
        "count_p90": jint(weighted_nearest_rank(&count_pairs, 0.90)),
        "count_p99": jint(weighted_nearest_rank(&count_pairs, 0.99)),
        "count_p999": jint(weighted_nearest_rank(&count_pairs, 0.999)),
        "lag1_autocorr": jnum(lag1),
        "run_p90": if run_pairs.is_empty() {
            Value::Null
        } else {
            jint(weighted_nearest_rank(&run_pairs, 0.90))
        },
    })
}

// -- Block 3 ----------------------------------------------------------------

/// One session's vote on a scale cell: `(robust, rms, return count)`.
type ScaleVote = (Option<f64>, Option<f64>, i64);
/// One session's vote on a horizon pair: `(vr, cov, cov norm, windows)`.
type PairVote = (Option<f64>, Option<f64>, Option<f64>, i64);

/// `aggregate_block3`: one vote per qualifying session, median across
/// sessions. Return counts are summed over the same qualifying votes.
#[must_use]
pub fn aggregate_block3(sessions: &[&Value]) -> Value {
    let mut cells: BTreeMap<(i64, i64), Vec<ScaleVote>> = BTreeMap::new();
    let mut pairs: BTreeMap<(i64, String), Vec<PairVote>> = BTreeMap::new();
    let mut lag1: BTreeMap<i64, Vec<Option<f64>>> = BTreeMap::new();
    let mut h20: BTreeMap<(String, i64), Vec<ScaleVote>> = BTreeMap::new();

    for rec in sessions {
        for (hour_s, per_h) in rec["cells"].as_object().expect("block3 cells") {
            for (h_s, c) in per_h.as_object().expect("a horizon map") {
                let h: i64 = h_s.parse().expect("a horizon key");
                let count = ji(c, "return_count");
                if count < horizon_floor(h) {
                    continue;
                }
                cells
                    .entry((hour_s.parse().expect("an hour key"), h))
                    .or_default()
                    .push((jf_opt(c, "robust_scale"), jf_opt(c, "rms_scale"), count));
            }
        }
        for (hour_s, per_pair) in rec["pairs"].as_object().expect("block3 pairs") {
            for (pair_s, pc) in per_pair.as_object().expect("a pair map") {
                let big: i64 = pair_s
                    .split('-')
                    .nth(1)
                    .expect("a pair key is h-big")
                    .parse()
                    .expect("the big horizon");
                let count = ji(pc, "window_count");
                if count < horizon_floor(big) {
                    continue;
                }
                pairs
                    .entry((hour_s.parse().expect("an hour key"), pair_s.clone()))
                    .or_default()
                    .push((
                        jf_opt(pc, "vr"),
                        jf_opt(pc, "cov_contrib"),
                        jf_opt(pc, "cov_contrib_norm"),
                        count,
                    ));
            }
        }
        for (hour_s, v) in rec["lag1_parent_autocorr"]
            .as_object()
            .expect("the lag-1 map")
        {
            lag1.entry(hour_s.parse().expect("an hour key"))
                .or_default()
                .push(super::opt_num(Some(v)));
        }
        for (lp, per_h) in rec["hour20_labels"].as_object().expect("hour20_labels") {
            for (h_s, c) in per_h.as_object().expect("a horizon map") {
                let h: i64 = h_s.parse().expect("a horizon key");
                // The boundary cells carry their own 60 s floor: an hour-20
                // label slice sees a fraction of the hour's boundaries.
                let floor = if h == 60 {
                    MIN_BOUNDARY_60S_CELL_RETURNS
                } else {
                    horizon_floor(h)
                };
                let count = ji(c, "return_count");
                if count < floor {
                    continue;
                }
                h20.entry((lp.clone(), h)).or_default().push((
                    jf_opt(c, "robust_scale"),
                    jf_opt(c, "rms_scale"),
                    count,
                ));
            }
        }
    }

    let mut cells_out = serde_json::Map::new();
    for hour in cells.keys().map(|k| k.0).collect::<BTreeSet<i64>>() {
        let mut inner = serde_json::Map::new();
        for &h in WALL_HORIZONS_S {
            let Some(votes) = cells.get(&(hour, h)) else {
                continue;
            };
            inner.insert(h.to_string(), scale_cell_json(votes));
        }
        cells_out.insert(hour.to_string(), Value::Object(inner));
    }
    let mut pairs_out = serde_json::Map::new();
    for hour in pairs.keys().map(|k| k.0).collect::<BTreeSet<i64>>() {
        let mut inner = serde_json::Map::new();
        for ((hh, pair_s), votes) in &pairs {
            if *hh != hour {
                continue;
            }
            inner.insert(
                pair_s.clone(),
                serde_json::json!({
                    "window_count": votes.iter().map(|v| v.3).sum::<i64>(),
                    "vr": jnum(median_or_none(&votes.iter().map(|v| v.0).collect::<Vec<_>>())),
                    "cov_contrib": jnum(median_or_none(
                        &votes.iter().map(|v| v.1).collect::<Vec<_>>())),
                    "cov_contrib_norm": jnum(median_or_none(
                        &votes.iter().map(|v| v.2).collect::<Vec<_>>())),
                }),
            );
        }
        pairs_out.insert(hour.to_string(), Value::Object(inner));
    }
    let lag1_out: serde_json::Map<String, Value> = lag1
        .iter()
        .map(|(&hour, vals)| (hour.to_string(), jnum(median_or_none(vals))))
        .collect();
    let mut h20_out = serde_json::Map::new();
    for lp in h20
        .keys()
        .map(|k| k.0.clone())
        .collect::<BTreeSet<String>>()
    {
        let mut inner = serde_json::Map::new();
        for ((lpp, h), votes) in &h20 {
            if *lpp != lp {
                continue;
            }
            inner.insert(h.to_string(), scale_cell_json(votes));
        }
        h20_out.insert(lp, Value::Object(inner));
    }
    serde_json::json!({
        "cells": Value::Object(cells_out),
        "pairs": Value::Object(pairs_out),
        "lag1_parent_autocorr": Value::Object(lag1_out),
        "hour20_labels": Value::Object(h20_out),
    })
}

fn scale_cell_json(votes: &[ScaleVote]) -> Value {
    serde_json::json!({
        "return_count": votes.iter().map(|v| v.2).sum::<i64>(),
        "robust_scale": jnum(median_or_none(&votes.iter().map(|v| v.0).collect::<Vec<_>>())),
        "rms_scale": jnum(median_or_none(&votes.iter().map(|v| v.1).collect::<Vec<_>>())),
    })
}

// -- Block 4 ----------------------------------------------------------------

/// The nine per-hour residual fields the monthly record medians.
const BLOCK4_FIELDS: [&str; 9] = [
    "zero_fraction",
    "nz_abs_p90",
    "nz_abs_p99",
    "nz_abs_p999",
    "ratio_p99_p90",
    "ratio_p999_p99",
    "exceed_4",
    "exceed_8",
    "exceed_16",
];

/// `aggregate_block4`: per hour key (string-sorted, so `"all"` lands last),
/// the median across sessions whose residual count reaches the floor, with
/// the counts summed over every session that carries the cell - qualifying
/// or not.
#[must_use]
pub fn aggregate_block4(sessions: &[&Value]) -> Value {
    let hours: BTreeSet<String> = sessions
        .iter()
        .flat_map(|rec| {
            rec.as_object()
                .expect("block4 is an object")
                .keys()
                .cloned()
        })
        .collect();
    let mut out = serde_json::Map::new();
    for hour in hours {
        let present: Vec<&Value> = sessions.iter().filter_map(|rec| rec.get(&hour)).collect();
        let qualifying: Vec<&&Value> = present
            .iter()
            .filter(|c| ji(c, "residual_count") >= MIN_RESIDUAL_CELL)
            .collect();
        let mut rec = serde_json::Map::new();
        rec.insert(
            "residual_count".into(),
            serde_json::json!(present.iter().map(|c| ji(c, "residual_count")).sum::<i64>()),
        );
        rec.insert(
            // The burn-in exclusion count, under its frozen artifact spelling.
            "warmup_excluded".into(),
            serde_json::json!(
                present
                    .iter()
                    .map(|c| ji(c, "warmup_excluded"))
                    .sum::<i64>()
            ),
        );
        for field in BLOCK4_FIELDS {
            let votes: Vec<Option<f64>> = qualifying.iter().map(|c| jf_opt(c, field)).collect();
            rec.insert(field.into(), jnum(median_or_none(&votes)));
        }
        out.insert(hour, Value::Object(rec));
    }
    Value::Object(out)
}

// -- Permutations -----------------------------------------------------------

/// `aggregate_permutations`: the Amendment-A session-hour combination. Per
/// session, the two segments' sufficient statistics are pooled (counts and
/// sums add, the max takes the max) and the floor applies to the combined
/// count; then the median across qualifying sessions per replicate index,
/// then the median across the 16 replicate medians.
#[must_use]
pub fn aggregate_permutations(per_session: &[&[Value]]) -> Value {
    /// `(variant, hour, horizon)`.
    type CellKey = (String, i64, i64);
    /// `(variant, hour, horizon, replicate)`.
    type RepKey = (String, i64, i64, i64);
    // The qualifying session votes, per cell and replicate index.
    let mut by_key: BTreeMap<CellKey, BTreeMap<i64, Vec<Option<f64>>>> = BTreeMap::new();
    for records in per_session {
        let mut combined: BTreeMap<RepKey, (i64, f64, f64)> = BTreeMap::new();
        for rec in *records {
            for h in [60i64, 300] {
                let key = (
                    js(rec, "variant").to_string(),
                    ji(rec, "hour"),
                    h,
                    ji(rec, "replicate"),
                );
                let acc = combined.entry(key).or_insert((0, 0.0, 0.0));
                acc.0 += ji(rec, &format!("return_count_{h}"));
                acc.1 += jf_opt(rec, &format!("sum_abs_{h}")).expect("sum_abs is a number");
                let m = jf_opt(rec, &format!("max_abs_{h}")).expect("max_abs is a number");
                if m > acc.2 {
                    acc.2 = m;
                }
            }
        }
        for ((variant, hour, h, rep), acc) in combined {
            if acc.0 < horizon_floor(h) {
                continue;
            }
            by_key
                .entry((variant, hour, h))
                .or_default()
                .entry(rep)
                .or_default()
                .push(robust_from_stats(acc.0, acc.1, acc.2));
        }
    }
    let mut out = serde_json::Map::new();
    for variant in PERMUTATION_VARIANTS {
        let hours: BTreeSet<i64> = by_key
            .keys()
            .filter(|k| k.0 == *variant)
            .map(|k| k.1)
            .collect();
        let mut vout = serde_json::Map::new();
        for hour in hours {
            let mut entry = serde_json::Map::new();
            for h in [60i64, 300] {
                let rep_medians: Vec<Option<f64>> = by_key
                    .get(&((*variant).to_string(), hour, h))
                    .map(|reps| reps.values().map(|vals| median_or_none(vals)).collect())
                    .unwrap_or_default();
                entry.insert(
                    format!("robust_scale_{h}"),
                    jnum(median_or_none(&rep_medians)),
                );
            }
            vout.insert(hour.to_string(), Value::Object(entry));
        }
        out.insert((*variant).to_string(), Value::Object(vout));
    }
    Value::Object(out)
}

/// The frozen replicate count, exposed so the caller can assert the cached
/// records carry a full permutation inventory.
pub const PERM_REPLICATES: i64 = PERMUTATION_REPLICATES;

// -- The four monthly blocks together ---------------------------------------

/// `{block1, block2, block3, block4}` over one side's per-session records -
/// the observed `monthly` and each seed's `blocks` are the same function.
pub fn blocks_from_sessions(per_session: &[Value]) -> LabResult<Value> {
    let pooled = pool_session_hists(per_session)?;
    let b2: Vec<&Value> = per_session.iter().map(|r| &r["block2"]).collect();
    let b3: Vec<&Value> = per_session.iter().map(|r| &r["block3"]).collect();
    let b4: Vec<&Value> = per_session.iter().map(|r| &r["block4"]).collect();
    Ok(serde_json::json!({
        "block1": block1_blocks(&pooled),
        "block2": pool_block2(&b2),
        "block3": aggregate_block3(&b3),
        "block4": aggregate_block4(&b4),
    }))
}

/// The Stage A projection's monthly `{block1, block2}` product. It accepts
/// reduced session records and deliberately shares both poolers with
/// [`blocks_from_sessions`], so the cadence-only oracle does not grow a
/// second aggregation implementation.
pub fn reduced_blocks_from_sessions(per_session: &[Value]) -> LabResult<Value> {
    let pooled = pool_session_hists(per_session)?;
    let b2: Vec<&Value> = per_session.iter().map(|r| &r["block2"]).collect();
    Ok(serde_json::json!({
        "block1": block1_blocks(&pooled),
        "block2": pool_block2(&b2),
    }))
}

// -- Central blocks (the 8-seed median tree) --------------------------------

/// `tree_median`: the recursive 8-seed median over identically shaped JSON
/// trees. A key-set mismatch refuses (the seeds run one code path over one
/// calendar, so shape drift is a defect); a numeric leaf where any seed is
/// null centralizes to null - never a median over fewer than the full seed
/// set; strings, booleans and arrays must agree exactly.
pub fn tree_median(trees: &[&Value]) -> LabResult<Value> {
    if trees.iter().all(|t| t.is_object()) {
        let key_sets: BTreeSet<Vec<&String>> = trees
            .iter()
            .map(|t| {
                let mut ks: Vec<&String> = t.as_object().expect("an object").keys().collect();
                ks.sort();
                ks
            })
            .collect();
        if key_sets.len() != 1 {
            let shown: Vec<Vec<&String>> = key_sets.into_iter().take(2).collect();
            return Err(LabError::refusal(format!(
                "generated seed trees diverge in shape: {shown:?}"
            )));
        }
        let mut out = serde_json::Map::new();
        for key in trees[0].as_object().expect("an object").keys() {
            let children: Vec<&Value> = trees.iter().map(|t| &t[key]).collect();
            out.insert(key.clone(), tree_median(&children)?);
        }
        return Ok(Value::Object(out));
    }
    if trees.iter().any(|t| t.is_null()) {
        return Ok(Value::Null);
    }
    if trees.iter().all(|t| t.is_number()) {
        // The median preserves the winning seed's leaf type: a histogram
        // count medians to an integer leaf, a scale to a float leaf.
        let mut sorted: Vec<&Value> = trees.to_vec();
        sorted.sort_by(|a, b| {
            a.as_f64()
                .expect("a number")
                .total_cmp(&b.as_f64().expect("a number"))
        });
        let n = sorted.len();
        let idx = if n % 2 == 1 { (n - 1) / 2 } else { n / 2 - 1 };
        return Ok(sorted[idx].clone());
    }
    let distinct: BTreeSet<String> = trees.iter().map(|t| canonical_text(t)).collect();
    if distinct.len() != 1 {
        let shown: Vec<&&Value> = trees.iter().take(2).collect();
        return Err(LabError::refusal(format!(
            "generated seed trees diverge on a non-numeric leaf: {shown:?}"
        )));
    }
    Ok(trees[0].clone())
}

/// The Python `json.dumps(t, sort_keys=True, default=str)` equality probe.
/// Only reached for strings, booleans and arrays, so key sorting is enough.
fn canonical_text(v: &Value) -> String {
    crate::kernel::typed_canon(v)
}

/// `central_blocks_from_seeds`: `SeedBlocks` minus `block1.hist`, every
/// scalar the 8-seed median.
///
/// The `Block2Cell` `count_hist` and `run_length_hist` maps are histograms
/// keyed by data-dependent support values, so their key sets legitimately
/// diverge across seeds. Per the signed union-zero-median ruling exactly
/// those two paths centralize over the union of the seed supports, an absent
/// support value reading as a zero count; every other dictionary keeps the
/// strict identical-shape refusal. Two properties of the Python are
/// deliberate and reproduced: the validation pass is complete before any
/// padding (a refusal leaves nothing padded) and the padding operates on
/// copies, so the per-seed evidence is never mutated.
pub fn central_blocks_from_seeds(seed_blocks: &[&Value]) -> LabResult<Value> {
    let mut stripped: Vec<Value> = Vec::with_capacity(seed_blocks.len());
    for b in seed_blocks {
        let mut copy = b.as_object().expect("seed blocks is an object").clone();
        let mut b1 = b["block1"]
            .as_object()
            .expect("block1 is an object")
            .clone();
        b1.remove("hist");
        copy.insert("block1".into(), Value::Object(b1));
        copy.insert("block2".into(), b["block2"].clone());
        stripped.push(Value::Object(copy));
    }
    let cells: BTreeSet<(String, String)> = stripped
        .iter()
        .flat_map(|b| {
            b["block2"]
                .as_object()
                .expect("block2 is an object")
                .iter()
                .flat_map(|(h, per_w)| {
                    per_w
                        .as_object()
                        .expect("a window map")
                        .keys()
                        .map(move |w| (h.clone(), w.clone()))
                })
        })
        .collect();
    // One complete validation pass over both fields and every present cell
    // before any padding.
    for (h, w) in &cells {
        for field in ["count_hist", "run_length_hist"] {
            for b in &stripped {
                let Some(cell) = b["block2"].get(h).and_then(|per_w| per_w.get(w)) else {
                    continue;
                };
                if cell.get(field).is_none() {
                    return Err(LabError::refusal(format!(
                        "a seed block2 cell hour {h} window {w} lacks {field}"
                    )));
                }
            }
        }
    }
    for (h, w) in &cells {
        for field in ["count_hist", "run_length_hist"] {
            let mut union: BTreeSet<String> = BTreeSet::new();
            for b in &stripped {
                if let Some(cell) = b["block2"].get(h).and_then(|per_w| per_w.get(w)) {
                    union.extend(
                        cell[field]
                            .as_object()
                            .expect("a histogram")
                            .keys()
                            .cloned(),
                    );
                }
            }
            for b in &mut stripped {
                let Some(cell) = b["block2"]
                    .get_mut(h)
                    .and_then(|per_w| per_w.get_mut(w))
                    .and_then(|c| c.get_mut(field))
                    .and_then(Value::as_object_mut)
                else {
                    continue;
                };
                for key in &union {
                    cell.entry(key.clone())
                        .or_insert_with(|| serde_json::json!(0));
                }
            }
        }
    }
    let refs: Vec<&Value> = stripped.iter().collect();
    tree_median(&refs)
}
