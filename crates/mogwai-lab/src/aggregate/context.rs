// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! [`ObsContext`]: the observed-side metric evaluators over a set of
//! per-session records, ported from `analysis/mnq_fit.py`'s class of the same
//! name. Every statistic is a function of a session-multiplicity vector, so
//! the point estimate (all ones), the 10,000 bootstrap replicates and the
//! leave-one-week folds all run one code path.
//!
//! Despite the name the class is used for both sides: the observed month is
//! one context resampled, and each generated seed is its own context always
//! evaluated at all-ones. That is deliberate - it is the same statistic on
//! both sides by construction, not by two implementations agreeing.
//!
//! ## The caching design is load-bearing, not an optimization detail
//!
//! Each accessor extracts its per-session votes (or a [`QuantileSupport`]
//! over the shared support) from the JSON records exactly once and memoizes
//! them; a replicate then costs a weighted median over 22 numbers or a
//! binary search. Without it, 10,000 replicates times ~40 metrics times a
//! re-walk of 22 sessions' block records would dominate everything else in
//! the phase. The Python's `self._cache` does the same job; this port keeps
//! the shape so the two stay comparable.
//!
//! ## The two strict accessors
//!
//! [`ObsContext::perm_value`] and [`ObsContext::b3_robust_strict`] are the
//! no-K-of-N rulings made executable: a missing or non-finite session vote
//! in ANY replicate - regardless of what the multiplicity vector would have
//! weighted to zero - refuses the whole statistic. They are separate methods
//! precisely so that the lenient path (`weighted_median_votes` over
//! `b3_votes`) cannot be reached by accident where the strict rule binds.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;

use super::bootstrap::{QuantileSupport, weighted_median_votes};
use super::monthly::{horizon_floor, robust_from_stats, sqrt_n_value};
use super::{ja, jf_opt, ji, ji_opt, js};
use crate::kernel::median_or_none;
use crate::session::parent_count_bin;
use crate::subcontract::{
    MIN_BOUNDARY_60S_CELL_RETURNS, MIN_RESIDUAL_CELL, PERMUTATION_REPLICATES,
};

/// A `(since_open_bin, until_close_bin)` filter.
pub type Labels = (&'static str, &'static str);

/// One cached per-session vote vector.
pub type Votes = Rc<Vec<Option<f64>>>;
/// A memoized accessor's backing store.
type Memo<K, V> = RefCell<HashMap<K, V>>;
/// The block-1 support key: `(field, hour, labels, parent-count bin)`.
type B1Key = (&'static str, Option<i64>, Option<Labels>, Option<String>);
/// One session's block-2 count histogram as `(total, sum, sumsq)`, or `None`
/// where the session serialized no such cell.
type B2Cells = Rc<Vec<Option<(i64, i64, i64)>>>;
/// The minute-count key: `(hour, labels)`, both optional filters.
type MinuteKey = (Option<i64>, Option<Labels>);

#[derive(Debug)]
pub struct ObsContext {
    per_session: Vec<Value>,
    /// The session labels, in record order - the index space every
    /// multiplicity vector is written against.
    pub sessions: Vec<String>,
    pub n: usize,
    b1_support: Memo<B1Key, Rc<QuantileSupport>>,
    b1_bin_count: Memo<(i64, String), Rc<Vec<i64>>>,
    b2_cells: Memo<(i64, i64), B2Cells>,
    b2_quant: Memo<(i64, i64), Rc<QuantileSupport>>,
    b3: Memo<(i64, i64, &'static str), Votes>,
    b3_cov: Memo<(i64, String), Votes>,
    b3_boundary: Memo<(String, i64, &'static str), Votes>,
    b4: Memo<(String, &'static str), Votes>,
    minute_counts: Memo<MinuteKey, Rc<Vec<i64>>>,
    perm: Memo<(&'static str, i64, i64, i64), Votes>,
}

impl ObsContext {
    #[must_use]
    pub fn new(per_session: Vec<Value>) -> Self {
        let sessions = per_session
            .iter()
            .map(|r| js(r, "session_date").to_string())
            .collect();
        let n = per_session.len();
        Self {
            per_session,
            sessions,
            n,
            b1_support: RefCell::default(),
            b1_bin_count: RefCell::default(),
            b2_cells: RefCell::default(),
            b2_quant: RefCell::default(),
            b3: RefCell::default(),
            b3_cov: RefCell::default(),
            b3_boundary: RefCell::default(),
            b4: RefCell::default(),
            minute_counts: RefCell::default(),
            perm: RefCell::default(),
        }
    }

    /// The all-ones multiplicity vector - the point estimate.
    #[must_use]
    pub fn ones(&self) -> Vec<i64> {
        vec![1; self.n]
    }

    #[must_use]
    pub fn per_session(&self) -> &[Value] {
        &self.per_session
    }

    // -- Block 1 quantiles --------------------------------------------------

    /// The pooled quantile support for one block-1 field under optional
    /// hour / label / parent-count-bin filters. `field` is `"trade"`
    /// (range ticks), `"quote"` (range half-ticks, valid-book minutes only)
    /// or `"sqrtn"` (trade ticks over `sqrt(n)`, `n >= 1` only).
    #[must_use]
    pub fn b1_support(
        &self,
        field: &'static str,
        hour: Option<i64>,
        labels: Option<Labels>,
        bin_name: Option<&str>,
    ) -> Rc<QuantileSupport> {
        let key = (field, hour, labels, bin_name.map(str::to_string));
        if let Some(hit) = self.b1_support.borrow().get(&key) {
            return Rc::clone(hit);
        }
        let per: Vec<Vec<(f64, i64)>> = self
            .per_session
            .iter()
            .map(|rec| {
                let mut pairs = Vec::new();
                for row in ja(rec, "block1_hist") {
                    if hour.is_some_and(|h| ji(row, "hour") != h) {
                        continue;
                    }
                    if let Some((s, u)) = labels
                        && (js(row, "since_open_bin") != s || js(row, "until_close_bin") != u)
                    {
                        continue;
                    }
                    let n = ji(row, "n");
                    if bin_name.is_some_and(|b| parent_count_bin(n) != b) {
                        continue;
                    }
                    let count = ji(row, "count");
                    let trade = ji(row, "trade_range_ticks");
                    match field {
                        "trade" => {
                            #[expect(
                                clippy::cast_precision_loss,
                                reason = "tick ranges stay small"
                            )]
                            pairs.push((trade as f64, count));
                        }
                        "quote" => {
                            if let Some(q) = ji_opt(row, "quote_range_half_ticks") {
                                #[expect(
                                    clippy::cast_precision_loss,
                                    reason = "half-tick ranges stay small"
                                )]
                                pairs.push((q as f64, count));
                            }
                        }
                        "sqrtn" => {
                            if n >= 1 {
                                pairs.push((sqrt_n_value(trade, n), count));
                            }
                        }
                        other => panic!("unknown block1 support field {other}"),
                    }
                }
                pairs
            })
            .collect();
        let built = Rc::new(QuantileSupport::new(&per));
        self.b1_support.borrow_mut().insert(key, Rc::clone(&built));
        built
    }

    /// The weighted populated-minute count of one `(hour, parent-count bin)`
    /// cell.
    #[must_use]
    pub fn b1_bin_count(&self, hour: i64, bin_name: &str, mult: &[i64]) -> i64 {
        let key = (hour, bin_name.to_string());
        let counts = self.b1_bin_count.borrow().get(&key).map(Rc::clone);
        let counts = counts.unwrap_or_else(|| {
            let built: Rc<Vec<i64>> = Rc::new(
                self.per_session
                    .iter()
                    .map(|rec| {
                        ja(rec, "block1_hist")
                            .iter()
                            .filter(|row| {
                                ji(row, "hour") == hour
                                    && parent_count_bin(ji(row, "n")) == bin_name
                            })
                            .map(|row| ji(row, "count"))
                            .sum()
                    })
                    .collect(),
            );
            self.b1_bin_count
                .borrow_mut()
                .insert(key, Rc::clone(&built));
            built
        });
        mult.iter().zip(counts.iter()).map(|(m, c)| m * c).sum()
    }

    // -- Block 2 ------------------------------------------------------------

    fn b2_cells(&self, hour: i64, w: i64) -> B2Cells {
        if let Some(hit) = self.b2_cells.borrow().get(&(hour, w)) {
            return Rc::clone(hit);
        }
        let mut cells = Vec::with_capacity(self.n);
        let mut supports = Vec::with_capacity(self.n);
        for rec in &self.per_session {
            let cell = rec["block2"]
                .get(hour.to_string())
                .and_then(|per_w| per_w.get(w.to_string()));
            match cell {
                None => {
                    cells.push(None);
                    supports.push(Vec::new());
                }
                Some(c) => {
                    let hist = c["count_hist"].as_object().expect("count_hist");
                    let mut total = 0i64;
                    let mut s = 0i64;
                    let mut sq = 0i64;
                    let mut pairs = Vec::with_capacity(hist.len());
                    for (k, v) in hist {
                        let k: i64 = k.parse().expect("a count key");
                        let v = v.as_i64().expect("a count");
                        total += v;
                        s += k * v;
                        sq += k * k * v;
                        #[expect(clippy::cast_precision_loss, reason = "window counts stay small")]
                        pairs.push((k as f64, v));
                    }
                    cells.push(Some((total, s, sq)));
                    supports.push(pairs);
                }
            }
        }
        let cells = Rc::new(cells);
        self.b2_cells
            .borrow_mut()
            .insert((hour, w), Rc::clone(&cells));
        self.b2_quant
            .borrow_mut()
            .insert((hour, w), Rc::new(QuantileSupport::new(&supports)));
        cells
    }

    /// The pooled Fano factor of one `(hour, window)` cell under `mult`.
    /// A nonpositive mean refuses rather than dividing.
    #[must_use]
    pub fn b2_fano(&self, hour: i64, w: i64, mult: &[i64]) -> Option<f64> {
        let cells = self.b2_cells(hour, w);
        let (mut total, mut s, mut sq) = (0i64, 0i64, 0i64);
        for (m, c) in mult.iter().zip(cells.iter()) {
            let (Some(c), true) = (c, *m != 0) else {
                continue;
            };
            total += m * c.0;
            s += m * c.1;
            sq += m * c.2;
        }
        if total == 0 {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "window counts stay far below 2^53"
        )]
        let (totalf, sf, sqf) = (total as f64, s as f64, sq as f64);
        let mean = sf / totalf;
        if mean <= 0.0 {
            return None;
        }
        let var = sqf / totalf - mean * mean;
        Some(var / mean)
    }

    /// The pooled per-window count quantile of one `(hour, window)` cell.
    #[must_use]
    pub fn b2_count_quantile(&self, hour: i64, w: i64, q: f64, mult: &[i64]) -> Option<f64> {
        self.b2_cells(hour, w);
        let support = Rc::clone(&self.b2_quant.borrow()[&(hour, w)]);
        support.quantile(q, mult)
    }

    // -- Block 3 session votes ----------------------------------------------

    /// One robust / RMS scale vote per session at `(hour, horizon)`, `None`
    /// where the session's cell is absent or below the horizon floor.
    #[must_use]
    pub fn b3_votes(&self, hour: i64, h: i64, stat: &'static str) -> Votes {
        if let Some(hit) = self.b3.borrow().get(&(hour, h, stat)) {
            return Rc::clone(hit);
        }
        let field = scale_field(stat);
        let floor = horizon_floor(h);
        let votes: Votes = Rc::new(
            self.per_session
                .iter()
                .map(|rec| {
                    let c = rec["block3"]["cells"]
                        .get(hour.to_string())
                        .and_then(|per_h| per_h.get(h.to_string()))?;
                    (ji(c, "return_count") >= floor)
                        .then(|| jf_opt(c, field))
                        .flatten()
                })
                .collect(),
        );
        self.b3
            .borrow_mut()
            .insert((hour, h, stat), Rc::clone(&votes));
        votes
    }

    /// One normalized covariance-contribution vote per session at
    /// `(hour, pair)`, qualified on the BIG horizon's floor.
    #[must_use]
    pub fn b3_cov_votes(&self, hour: i64, pair: &str) -> Votes {
        let key = (hour, pair.to_string());
        if let Some(hit) = self.b3_cov.borrow().get(&key) {
            return Rc::clone(hit);
        }
        let big: i64 = pair
            .split('-')
            .nth(1)
            .expect("a pair key is h-big")
            .parse()
            .expect("the big horizon");
        let floor = horizon_floor(big);
        let votes: Votes = Rc::new(
            self.per_session
                .iter()
                .map(|rec| {
                    let pc = rec["block3"]["pairs"]
                        .get(hour.to_string())
                        .and_then(|per_pair| per_pair.get(pair))?;
                    (ji(pc, "window_count") >= floor)
                        .then(|| jf_opt(pc, "cov_contrib_norm"))
                        .flatten()
                })
                .collect(),
        );
        self.b3_cov.borrow_mut().insert(key, Rc::clone(&votes));
        votes
    }

    /// One hour-20 label-slice scale vote per session. The 60 s horizon
    /// carries its OWN boundary floor here, not the horizon's.
    #[must_use]
    pub fn b3_boundary_votes(&self, label_pair: &str, h: i64, stat: &'static str) -> Votes {
        let key = (label_pair.to_string(), h, stat);
        if let Some(hit) = self.b3_boundary.borrow().get(&key) {
            return Rc::clone(hit);
        }
        let floor = if h == 60 {
            MIN_BOUNDARY_60S_CELL_RETURNS
        } else {
            horizon_floor(h)
        };
        let field = scale_field(stat);
        let votes: Votes = Rc::new(
            self.per_session
                .iter()
                .map(|rec| {
                    let c = rec["block3"]["hour20_labels"]
                        .get(label_pair)
                        .and_then(|per_h| per_h.get(h.to_string()))?;
                    (ji(c, "return_count") >= floor)
                        .then(|| jf_opt(c, field))
                        .flatten()
                })
                .collect(),
        );
        self.b3_boundary.borrow_mut().insert(key, Rc::clone(&votes));
        votes
    }

    // -- Block 4 session votes ----------------------------------------------

    /// One residual-field vote per session at an hour key (`"0".."23"` or
    /// the pooled `"all"`), qualified on `MIN_RESIDUAL_CELL`.
    #[must_use]
    pub fn b4_votes(&self, hour_key: &str, field: &'static str) -> Votes {
        let key = (hour_key.to_string(), field);
        if let Some(hit) = self.b4.borrow().get(&key) {
            return Rc::clone(hit);
        }
        let votes: Votes = Rc::new(
            self.per_session
                .iter()
                .map(|rec| {
                    let c = rec["block4"].get(hour_key)?;
                    (ji(c, "residual_count") >= MIN_RESIDUAL_CELL)
                        .then(|| jf_opt(c, field))
                        .flatten()
                })
                .collect(),
        );
        self.b4.borrow_mut().insert(key, Rc::clone(&votes));
        votes
    }

    // -- Q1 qualification inputs --------------------------------------------

    /// The populated-minute count per session under optional hour / label
    /// filters - the Q1 floor input, unweighted.
    #[must_use]
    pub fn minute_counts(&self, hour: Option<i64>, labels: Option<Labels>) -> Rc<Vec<i64>> {
        let key = (hour, labels);
        if let Some(hit) = self.minute_counts.borrow().get(&key) {
            return Rc::clone(hit);
        }
        let counts: Rc<Vec<i64>> = Rc::new(
            self.per_session
                .iter()
                .map(|rec| {
                    ja(rec, "block1_hist")
                        .iter()
                        .filter(|row| hour.is_none_or(|h| ji(row, "hour") == h))
                        .filter(|row| {
                            labels.is_none_or(|(s, u)| {
                                js(row, "since_open_bin") == s && js(row, "until_close_bin") == u
                            })
                        })
                        .map(|row| ji(row, "count"))
                        .sum()
                })
                .collect(),
        );
        self.minute_counts
            .borrow_mut()
            .insert(key, Rc::clone(&counts));
        counts
    }

    /// The serialized scheduled-window count per session at `(hour, w)`,
    /// `None` where the session serialized no such cell.
    #[must_use]
    pub fn b2_scheduled(&self, hour: i64, w: i64) -> Vec<Option<i64>> {
        self.per_session
            .iter()
            .map(|rec| {
                rec["block2"]
                    .get(hour.to_string())
                    .and_then(|per_w| per_w.get(w.to_string()))
                    .map(|c| ji(c, "scheduled_windows"))
            })
            .collect()
    }

    // -- Permutation session values -----------------------------------------

    /// One session-hour robust scale per session for one replicate index
    /// (Amendment A): the segment records are combined by count sum,
    /// `sum_abs` sum and `max_abs` max, and the floor applies to the
    /// combined count.
    #[must_use]
    pub fn perm_votes(&self, variant: &'static str, hour: i64, h: i64, rep: i64) -> Votes {
        let key = (variant, hour, h, rep);
        if let Some(hit) = self.perm.borrow().get(&key) {
            return Rc::clone(hit);
        }
        let floor = horizon_floor(h);
        let count_key = format!("return_count_{h}");
        let sum_key = format!("sum_abs_{h}");
        let max_key = format!("max_abs_{h}");
        let votes: Votes = Rc::new(
            self.per_session
                .iter()
                .map(|rec| {
                    let mut cnt = 0i64;
                    let mut s_abs = 0.0f64;
                    let mut m_abs = 0.0f64;
                    for p in ja(rec, "permutations") {
                        if js(p, "variant") != variant
                            || ji(p, "hour") != hour
                            || ji(p, "replicate") != rep
                        {
                            continue;
                        }
                        cnt += ji(p, &count_key);
                        s_abs += jf_opt(p, &sum_key).expect("sum_abs is a number");
                        let m = jf_opt(p, &max_key).expect("max_abs is a number");
                        if m > m_abs {
                            m_abs = m;
                        }
                    }
                    if cnt < floor {
                        None
                    } else {
                        robust_from_stats(cnt, s_abs, m_abs)
                    }
                })
                .collect(),
        );
        self.perm.borrow_mut().insert(key, Rc::clone(&votes));
        votes
    }

    /// The strict counterfactual statistic: the pseudo-month is evaluated
    /// under all 16 replicate indices and its value is their median.
    ///
    /// Q1 strictness, the no-K-of-N ruling: a missing or non-finite session
    /// vote in any replicate - regardless of the multiplicity vector, so
    /// including sessions this replicate weights to zero - refuses the whole
    /// statistic, as does a missing per-replicate median.
    #[must_use]
    pub fn perm_value(
        &self,
        variant: &'static str,
        hour: i64,
        h: i64,
        mult: &[i64],
    ) -> Option<f64> {
        let mut per_rep = Vec::with_capacity(PERMUTATION_REPLICATES as usize);
        for rep in 0..PERMUTATION_REPLICATES {
            let votes = self.perm_votes(variant, hour, h, rep);
            if votes.iter().any(|v| !v.is_some_and(f64::is_finite)) {
                return None;
            }
            let med = weighted_median_votes(&votes, mult);
            if !med.is_some_and(f64::is_finite) {
                return None;
            }
            per_rep.push(med);
        }
        median_or_none(&per_rep)
    }

    /// The strict robust scale the closure and worsening statistics consume:
    /// any missing or non-finite session vote refuses outright, regardless
    /// of the bootstrap or fold multiplicities.
    #[must_use]
    pub fn b3_robust_strict(&self, hour: i64, h: i64, mult: &[i64]) -> Option<f64> {
        let votes = self.b3_votes(hour, h, "robust");
        if votes.iter().any(|v| !v.is_some_and(f64::is_finite)) {
            return None;
        }
        weighted_median_votes(&votes, mult)
    }
}

fn scale_field(stat: &str) -> &'static str {
    if stat == "robust" {
        "robust_scale"
    } else {
        "rms_scale"
    }
}
