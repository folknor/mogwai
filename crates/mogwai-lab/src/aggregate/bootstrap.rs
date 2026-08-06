// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The resampling machinery (spec 6.1): the fixed-seed circular moving-block
//! bootstrap, the leave-one-ISO-week-out folds, the multiplicity-weighted
//! nearest-rank median and the cumulative-count [`QuantileSupport`] that
//! makes 10,000 replicates of a pooled quantile tractable.
//!
//! Ported from `analysis/mnq_fit.py`'s `bootstrap_multiplicities`,
//! `fold_multiplicities`, `weighted_median_votes` and `class
//! QuantileSupport`. Every one of these is bit-reproducible by construction:
//! the block starts come from `splitmix64` over a frozen base seed, and no
//! statistic anywhere consults a clock, a hash order or an address.

use crate::kernel::splitmix64;
use crate::session::{civil_from_days, days_from_civil, days_from_iso};
use crate::subcontract::{
    BOOTSTRAP_BASE_SEED, BOOTSTRAP_BLOCK_SESSIONS, BOOTSTRAP_REPLICATES, FOLD_MIN_SESSIONS,
};

/// One replicate's session multiplicity vector.
pub type Mult = Vec<i64>;

/// `bootstrap_multiplicities`: per replicate, the multiplicity vector of one
/// pseudo-month - sessions sorted ascending, exactly five circular block
/// starts of five consecutive sessions each, concatenated in draw order and
/// truncated to `n_sessions`.
///
/// `start = splitmix64(BASE xor (replicate << 8) xor block) mod n_sessions`.
/// The shift is a PYTHON shift on an unbounded integer masked back to 64
/// bits afterwards, so replicate 10,000 does not alias replicate 0 - the
/// mask is applied to the whole xor, exactly as the Python writes it.
#[must_use]
pub fn bootstrap_multiplicities(n_sessions: usize) -> Vec<Mult> {
    assert!(n_sessions > 0, "a bootstrap needs at least one session");
    let base = BOOTSTRAP_BASE_SEED as u64;
    let mut out = Vec::with_capacity(BOOTSTRAP_REPLICATES as usize);
    for rep in 0..BOOTSTRAP_REPLICATES {
        let mut mult = vec![0i64; n_sessions];
        let mut drawn = 0usize;
        for block in 0..5u64 {
            let shifted = (rep as u64) << 8;
            let start = (splitmix64(base ^ shifted ^ block) % n_sessions as u64) as usize;
            for k in 0..BOOTSTRAP_BLOCK_SESSIONS as usize {
                if drawn >= n_sessions {
                    break;
                }
                mult[(start + k) % n_sessions] += 1;
                drawn += 1;
            }
        }
        out.push(mult);
    }
    out
}

/// `fold_multiplicities`: leave-one-ISO-week-out 0/1 vectors over the
/// session labels IN THEIR GIVEN ORDER. A fold qualifies only when at least
/// `FOLD_MIN_SESSIONS` sessions remain, and a partial week is its own fold.
#[must_use]
pub fn fold_multiplicities(sessions: &[String]) -> Vec<Mult> {
    let mut weeks: std::collections::BTreeMap<(i64, u32), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, label) in sessions.iter().enumerate() {
        weeks.entry(iso_year_week(label)).or_default().push(i);
    }
    let mut folds = Vec::new();
    for idxs in weeks.values() {
        if (sessions.len() - idxs.len()) as i64 >= FOLD_MIN_SESSIONS {
            let mut mult = vec![1i64; sessions.len()];
            for &i in idxs {
                mult[i] = 0;
            }
            folds.push(mult);
        }
    }
    folds
}

/// `datetime.date.fromisoformat(label).isocalendar()[:2]`: the ISO week-year
/// and week number of a `"YYYY-MM-DD"` label. ISO weeks start on Monday and
/// week 1 is the week containing the first Thursday, which is why the
/// week-year can differ from the calendar year at a year boundary.
#[must_use]
pub fn iso_year_week(label: &str) -> (i64, u32) {
    let day = days_from_iso(label);
    // Day-of-week with Monday = 0 (1970-01-01 was a Thursday, index 3).
    let dow = (day + 3).rem_euclid(7);
    // The Thursday of this ISO week decides the week-year.
    let thursday = day - dow + 3;
    let (year, _, _) = civil_from_days(thursday);
    let jan1 = days_from_civil(year, 1, 1);
    let week = ((thursday - jan1) / 7 + 1) as u32;
    (year, week)
}

/// `weighted_median_votes`: the nearest-rank median of the session votes
/// under a replicate's multiplicities, i.e. the `ceil(total/2)`-th order
/// statistic of the multiset. A `None` vote (a non-qualifying session)
/// contributes nothing rather than refusing - the STRICT accessors on
/// [`super::context::ObsContext`] are what refuse.
#[must_use]
pub fn weighted_median_votes(values: &[Option<f64>], mult: &[i64]) -> Option<f64> {
    let mut pairs: Vec<(f64, i64)> = values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| match v {
            Some(x) if mult[i] > 0 => Some((*x, mult[i])),
            _ => None,
        })
        .collect();
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let total: i64 = pairs.iter().map(|p| p.1).sum();
    if total == 0 {
        return None;
    }
    let target = total.div_euclid(2) + total.rem_euclid(2);
    let mut cum = 0i64;
    for (v, w) in &pairs {
        cum += w;
        if cum >= target {
            return Some(*v);
        }
    }
    pairs.last().map(|p| p.0)
}

/// `QuantileSupport`: per-session cumulative counts over a SHARED sorted
/// support, so a pooled quantile under a multiplicity vector costs one
/// binary search over the support rather than a re-pooling of every
/// session's histogram. This is the structure that makes 10,000 replicates
/// of the block-1 and block-2 quantiles tractable.
#[derive(Debug)]
pub struct QuantileSupport {
    support: Vec<f64>,
    /// `cum[session][i]` = weight at or below `support[i]` for that session.
    cum: Vec<Vec<i64>>,
    totals: Vec<i64>,
}

impl QuantileSupport {
    /// Build from one `(value, weight)` list per session.
    #[must_use]
    pub fn new(per_session_pairs: &[Vec<(f64, i64)>]) -> Self {
        let mut support: Vec<f64> = per_session_pairs
            .iter()
            .flat_map(|pairs| pairs.iter().map(|p| p.0))
            .collect();
        support.sort_by(f64::total_cmp);
        support.dedup_by(|a, b| a.to_bits() == b.to_bits());
        let index: std::collections::HashMap<u64, usize> = support
            .iter()
            .enumerate()
            .map(|(i, v)| (v.to_bits(), i))
            .collect();
        let mut cum = Vec::with_capacity(per_session_pairs.len());
        let mut totals = Vec::with_capacity(per_session_pairs.len());
        for pairs in per_session_pairs {
            let mut arr = vec![0i64; support.len()];
            for (v, w) in pairs {
                arr[index[&v.to_bits()]] += w;
            }
            for i in 1..support.len() {
                arr[i] += arr[i - 1];
            }
            totals.push(arr.last().copied().unwrap_or(0));
            cum.push(arr);
        }
        Self {
            support,
            cum,
            totals,
        }
    }

    /// The pooled nearest-rank quantile under `mult`: the smallest support
    /// value whose weighted cumulative count reaches `q * total`. An empty
    /// support or a zero total refuses.
    #[must_use]
    pub fn quantile(&self, q: f64, mult: &[i64]) -> Option<f64> {
        if self.support.is_empty() {
            return None;
        }
        let total: i64 = mult.iter().zip(&self.totals).map(|(m, t)| m * t).sum();
        if total <= 0 {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "minute counts stay far below 2^53"
        )]
        let target = q * total as f64;
        let (mut lo, mut hi) = (0usize, self.support.len() - 1);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let cum: i64 = mult
                .iter()
                .zip(&self.cum)
                .filter(|(m, _)| **m != 0)
                .map(|(m, arr)| m * arr[mid])
                .sum();
            #[expect(clippy::cast_precision_loss, reason = "see above")]
            let cum_f = cum as f64;
            if cum_f >= target {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        Some(self.support[lo])
    }
}
