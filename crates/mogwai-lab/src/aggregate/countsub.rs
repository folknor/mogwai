// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The spec-5.2 count substitution and the 5.3 gap closure: reweight each
//! generated hour's populated minutes so its parent-count-bin frequencies
//! match the observed shares, preserving the hour's total weight, then read
//! one full-month weighted minute-range p99.9 and `> 968` exceedance rate
//! off the counterfactual.
//!
//! Ported from the retired Python fit implementation's `observed_bin_shares`,
//! `count_substitution`, `class CountSubEval`, `obs_shares_under`,
//! `count_substitution_closures` and `gap_closure`.
//!
//! ## The frozen weight edge cases
//!
//! With `o` the observed share of a bin and `g` the generated share:
//!
//! | `o` | `g` | weight |
//! |---|---|---|
//! | `> 0` | `> 0` | `o / g`, rescaled to preserve the hour's total |
//! | `= 0` | `> 0` | `0` - the generated month over-populates a bin the month never showed |
//! | `= 0` | `= 0` | `null`, the bin is ignored (not zero-weighted) |
//! | `> 0` | `= 0` | a support refusal: the whole hour fails |
//!
//! A hour with observed minutes and no generated support at all refuses the
//! same way (the union rule), and a nonpositive rescaling sum refuses too. A
//! refused hour is dropped from the pooled counterfactual entirely - never
//! partially substituted.
//!
//! ## Why the accumulation order is pinned
//!
//! `total_w`, `exceed_w` and the weighted cumulative counts are float sums
//! over the pooled histogram and over the weight map. The Python accumulates
//! them in `dict` insertion order, so this port walks
//! [`super::monthly::PooledHist`] and its own insertion-ordered weight map in
//! exactly that order. Sorting them instead would move the last ulp of
//! `counterfactual_exceed_968`.

use std::collections::HashMap;

use serde_json::Value;

use super::context::ObsContext;
use super::monthly::PooledHist;
use super::{RefusalRec, jnum};
use crate::kernel::{median_or_none, nearest_rank_p};
use crate::session::parent_count_bin;
use crate::subcontract::{GAP_CLOSE_EPS, PARENT_COUNT_BIN_NAMES};

/// `gap_closure` (spec 5.3): how much of the generated-to-observed log gap a
/// counterfactual closes. Refused on a missing or nonpositive input and on a
/// denominator below `GAP_CLOSE_EPS` - never a division through a vanishing
/// gap.
#[must_use]
pub fn gap_closure(
    t_gen: Option<f64>,
    t_cf: Option<f64>,
    t_obs: Option<f64>,
    generated_side: bool,
) -> Option<f64> {
    let (g, c, o) = (t_gen?, t_cf?, t_obs?);
    if g <= 0.0 || c <= 0.0 || o <= 0.0 {
        return None;
    }
    let denom = g.ln() - o.ln();
    if denom.abs() < GAP_CLOSE_EPS {
        return None;
    }
    if generated_side {
        Some((g.ln() - c.ln()) / denom)
    } else {
        Some((c.ln() - o.ln()) / denom)
    }
}

/// `obs_shares_under`: the observed populated-minute share of each
/// parent-count bin within each hour, under a multiplicity vector. Hours
/// with no populated minutes are absent from the map, which is what makes
/// them an hour-union support refusal on the generated side.
#[must_use]
pub fn obs_shares_under(obs: &ObsContext, mult: &[i64]) -> HashMap<i64, HashMap<String, f64>> {
    let mut shares = HashMap::new();
    for hour in 0..24i64 {
        let mut per = HashMap::new();
        let mut total = 0i64;
        for &b in PARENT_COUNT_BIN_NAMES {
            let c = obs.b1_bin_count(hour, b, mult);
            per.insert((*b).to_string(), c);
            total += c;
        }
        if total != 0 {
            #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
            let totalf = total as f64;
            shares.insert(
                hour,
                per.into_iter()
                    .map(|(b, c)| {
                        #[expect(clippy::cast_precision_loss, reason = "see above")]
                        let v = c as f64 / totalf;
                        (b, v)
                    })
                    .collect(),
            );
        }
    }
    shares
}

/// `observed_bin_shares`: the same shares straight off a pooled histogram
/// (the all-ones case the serialized per-seed record reports).
#[must_use]
pub fn observed_bin_shares(pooled: &PooledHist) -> HashMap<i64, HashMap<String, f64>> {
    let mut counts: HashMap<i64, HashMap<String, i64>> = HashMap::new();
    for (k, c) in pooled.iter() {
        *counts
            .entry(k.3)
            .or_default()
            .entry(parent_count_bin(k.0).to_string())
            .or_insert(0) += c;
    }
    counts
        .into_iter()
        .map(|(h, per_bin)| {
            #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
            let total = per_bin.values().sum::<i64>() as f64;
            let per = per_bin
                .into_iter()
                .map(|(b, n)| {
                    #[expect(clippy::cast_precision_loss, reason = "see above")]
                    let v = n as f64 / total;
                    (b, v)
                })
                .collect();
            (h, per)
        })
        .collect()
}

/// The `(value, weight)` nearest rank with float weights, returning the
/// integer support value. The frozen 5.2 rule, over a weighted histogram.
fn weighted_nearest_rank_fw(pairs: &mut [(i64, f64)], q: f64) -> Option<i64> {
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let total = crate::kernel::py_sum(pairs.iter().map(|p| p.1));
    if total <= 0.0 {
        return None;
    }
    let target = q * total;
    let mut cum = 0.0f64;
    for (v, w) in pairs.iter() {
        cum += w;
        if cum >= target {
            return Some(*v);
        }
    }
    pairs.last().map(|p| p.0)
}

/// `count_substitution`: one seed's serialized counterfactual record, over
/// the all-ones observed shares.
#[must_use]
pub fn count_substitution(
    gen_hist: &PooledHist,
    obs_shares: &HashMap<i64, HashMap<String, f64>>,
) -> Value {
    // Generated populated-minute counts per (hour, bin), in hour-first
    // appearance order for the float accumulation below.
    let mut gen_counts: HashMap<i64, HashMap<String, i64>> = HashMap::new();
    for (k, c) in gen_hist.iter() {
        *gen_counts
            .entry(k.3)
            .or_default()
            .entry(parent_count_bin(k.0).to_string())
            .or_insert(0) += c;
    }

    let mut weights: std::collections::BTreeMap<i64, HashMap<String, Option<f64>>> =
        std::collections::BTreeMap::new();
    let mut refused_hours: Vec<i64> = Vec::new();
    let mut support_refusals: Vec<RefusalRec> = Vec::new();

    let mut hours: Vec<i64> = gen_counts.keys().copied().collect();
    hours.extend(obs_shares.keys().copied());
    hours.sort_unstable();
    hours.dedup();

    for h in hours {
        let empty_g = HashMap::new();
        let g_per = gen_counts.get(&h).unwrap_or(&empty_g);
        let g_total: i64 = g_per.values().sum();
        let empty_o = HashMap::new();
        let o_per = obs_shares.get(&h).unwrap_or(&empty_o);
        let mut w_per: HashMap<String, Option<f64>> = HashMap::new();
        // `raw` keeps the frozen bin order, because the rescaling sum below
        // is a float accumulation over it.
        let mut raw: Vec<(&'static str, f64)> = Vec::new();
        let mut refused = false;
        for &b in PARENT_COUNT_BIN_NAMES {
            let o = o_per.get(b).copied().unwrap_or(0.0);
            #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
            let g = if g_total == 0 {
                0.0
            } else {
                g_per.get(b).copied().unwrap_or(0) as f64 / g_total as f64
            };
            if o > 0.0 && g > 0.0 {
                raw.push((b, o / g));
            } else if o == 0.0 && g > 0.0 {
                raw.push((b, 0.0));
            } else if o == 0.0 && g == 0.0 {
                w_per.insert((*b).to_string(), None); // the bin is ignored
            } else {
                refused = true;
                support_refusals.push(RefusalRec::new(
                    "count_substitution",
                    format!("hour {h} bin {b}"),
                    "observed support with zero generated support",
                ));
            }
        }
        if refused || g_total == 0 {
            // A generated-supported hour with a refusing bin, or an observed
            // hour with no generated support at all: both refuse the hour
            // (the 5.2 union rule). An hour with neither is simply absent.
            if g_total != 0 || !o_per.is_empty() {
                refused_hours.push(h);
            }
            weights.insert(h, all_none());
            continue;
        }
        #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
        let wsum = crate::kernel::py_sum(
            raw.iter()
                .map(|(b, w)| w * g_per.get(*b).copied().unwrap_or(0) as f64),
        );
        #[expect(clippy::cast_precision_loss, reason = "see above")]
        let scale = (wsum > 0.0).then(|| g_total as f64 / wsum);
        for (b, w) in &raw {
            w_per.insert((*b).to_string(), scale.map(|s| w * s));
        }
        if scale.is_none() {
            refused_hours.push(h);
            w_per = all_none();
        }
        weights.insert(
            h,
            PARENT_COUNT_BIN_NAMES
                .iter()
                .map(|b| ((*b).to_string(), w_per.get(*b).copied().flatten()))
                .collect(),
        );
    }

    // Pool every live hour, preserving the generated hour mixture.
    let mut pairs: Vec<(i64, f64)> = Vec::new();
    let mut total_w = 0.0f64;
    let mut exceed_w = 0.0f64;
    for (k, c) in gen_hist.iter() {
        let h = k.3;
        if refused_hours.contains(&h) {
            continue;
        }
        let b = parent_count_bin(k.0);
        let Some(Some(w)) = weights.get(&h).map(|per| per.get(b).copied().flatten()) else {
            continue;
        };
        #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
        let wt = w * c as f64;
        pairs.push((k.2, wt));
        total_w += wt;
        if k.2 > 968 {
            exceed_w += wt;
        }
    }
    let cf_p999 = weighted_nearest_rank_fw(&mut pairs, 0.999);

    let mut obs_hours: Vec<i64> = obs_shares.keys().copied().collect();
    obs_hours.sort_unstable();
    let shares_observed: serde_json::Map<String, Value> = obs_hours
        .iter()
        .map(|h| {
            let per = &obs_shares[h];
            let inner: serde_json::Map<String, Value> = PARENT_COUNT_BIN_NAMES
                .iter()
                .map(|b| {
                    (
                        (*b).to_string(),
                        serde_json::json!(per.get(*b).copied().unwrap_or(0.0)),
                    )
                })
                .collect();
            (h.to_string(), Value::Object(inner))
        })
        .collect();
    let mut gen_hours: Vec<i64> = gen_counts.keys().copied().collect();
    gen_hours.sort_unstable();
    let shares_generated: serde_json::Map<String, Value> = gen_hours
        .iter()
        .map(|h| {
            let per = &gen_counts[h];
            #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
            let total = per.values().sum::<i64>() as f64;
            let inner: serde_json::Map<String, Value> = PARENT_COUNT_BIN_NAMES
                .iter()
                .map(|b| {
                    #[expect(clippy::cast_precision_loss, reason = "see above")]
                    let v = if total == 0.0 {
                        0.0
                    } else {
                        per.get(*b).copied().unwrap_or(0) as f64 / total
                    };
                    ((*b).to_string(), serde_json::json!(v))
                })
                .collect();
            (h.to_string(), Value::Object(inner))
        })
        .collect();
    let weights_out: serde_json::Map<String, Value> = weights
        .iter()
        .map(|(h, per)| {
            let inner: serde_json::Map<String, Value> = PARENT_COUNT_BIN_NAMES
                .iter()
                .map(|b| ((*b).to_string(), jnum(per.get(*b).copied().flatten())))
                .collect();
            (h.to_string(), Value::Object(inner))
        })
        .collect();
    refused_hours.sort_unstable();

    serde_json::json!({
        "shares_observed": Value::Object(shares_observed),
        "shares_generated": Value::Object(shares_generated),
        "weights": Value::Object(weights_out),
        "refused_hours": refused_hours,
        "support_refusals": support_refusals.iter().map(RefusalRec::to_json).collect::<Vec<_>>(),
        "counterfactual_p999": super::jint(cf_p999),
        "counterfactual_exceed_968": jnum((total_w > 0.0).then(|| exceed_w / total_w)),
    })
}

fn all_none() -> HashMap<String, Option<f64>> {
    PARENT_COUNT_BIN_NAMES
        .iter()
        .map(|b| ((*b).to_string(), None))
        .collect()
}

/// The support refusals a seed's substitution produced, for 2c's
/// `refused_cells` collection.
#[must_use]
pub fn support_refusals_of(record: &Value) -> Vec<RefusalRec> {
    record["support_refusals"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    RefusalRec::new(
                        super::js(r, "scope"),
                        super::js(r, "cell"),
                        super::js(r, "reason"),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `CountSubEval`: one seed's generated support, precomputed so that a
/// counterfactual under a resampled observed share vector costs one binary
/// search over the shared trade-range support rather than a re-pooling.
/// This is what makes rung 2b's 10,000 replicates times 8 seeds tractable.
#[derive(Debug)]
pub struct CountSubEval {
    support: Vec<i64>,
    /// Per `(hour, bin)` group, in first-appearance order: the cumulative
    /// count over `support`, the group total and the `> 968` count.
    groups: Vec<(i64, &'static str, Vec<i64>, i64, i64)>,
    /// `(hour, total)` in first-appearance order.
    hour_totals: Vec<(i64, i64)>,
    gen_shares: HashMap<(i64, &'static str), f64>,
    /// `(hour, bin) -> index into `groups``. A linear scan here would cost
    /// 10,000 replicates times 8 seeds times a hundred groups.
    slot: HashMap<(i64, &'static str), usize>,
    hours: std::collections::HashSet<i64>,
}

impl CountSubEval {
    #[must_use]
    pub fn new(gen_hist: &PooledHist) -> Self {
        let mut support: Vec<i64> = gen_hist.iter().map(|(k, _)| k.2).collect();
        support.sort_unstable();
        support.dedup();
        let index: HashMap<i64, usize> = support.iter().enumerate().map(|(i, v)| (*v, i)).collect();
        let mut order: Vec<(i64, &'static str)> = Vec::new();
        let mut slot: HashMap<(i64, &'static str), usize> = HashMap::new();
        let mut arrays: Vec<Vec<i64>> = Vec::new();
        let mut totals: Vec<i64> = Vec::new();
        let mut exceeds: Vec<i64> = Vec::new();
        for (k, c) in gen_hist.iter() {
            let g = (k.3, parent_count_bin(k.0));
            let i = *slot.entry(g).or_insert_with(|| {
                order.push(g);
                arrays.push(vec![0; support.len()]);
                totals.push(0);
                exceeds.push(0);
                order.len() - 1
            });
            arrays[i][index[&k.2]] += c;
            totals[i] += c;
            if k.2 > 968 {
                exceeds[i] += c;
            }
        }
        for arr in &mut arrays {
            for i in 1..support.len() {
                arr[i] += arr[i - 1];
            }
        }
        let mut hour_totals: Vec<(i64, i64)> = Vec::new();
        let mut hour_slot: HashMap<i64, usize> = HashMap::new();
        for (i, (h, _)) in order.iter().enumerate() {
            let j = *hour_slot.entry(*h).or_insert_with(|| {
                hour_totals.push((*h, 0));
                hour_totals.len() - 1
            });
            hour_totals[j].1 += totals[i];
        }
        let mut gen_shares = HashMap::new();
        for (i, g) in order.iter().enumerate() {
            let ht = hour_totals[hour_slot[&g.0]].1;
            #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
            let share = totals[i] as f64 / ht as f64;
            gen_shares.insert(*g, share);
        }
        let groups: Vec<(i64, &'static str, Vec<i64>, i64, i64)> = order
            .iter()
            .enumerate()
            .map(|(i, (h, b))| (*h, *b, arrays[i].clone(), totals[i], exceeds[i]))
            .collect();
        let hours = hour_totals.iter().map(|(h, _)| *h).collect();
        Self {
            support,
            groups,
            hour_totals,
            gen_shares,
            slot,
            hours,
        }
    }

    /// `(p999, exceed rate, refused hours)` under an observed share vector.
    /// Hours are the union of observed and generated: an observed hour the
    /// generated month never populates is itself a support refusal.
    #[must_use]
    pub fn counterfactual(
        &self,
        obs_shares: &HashMap<i64, HashMap<String, f64>>,
    ) -> (Option<i64>, Option<f64>, Vec<i64>) {
        let mut refused_hours: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
        for h in obs_shares.keys() {
            if !self.hours.contains(h) {
                refused_hours.insert(*h);
            }
        }
        // The weight map keeps the hour-then-bin order the totals were built
        // in - the float sums below walk it.
        let mut weights: Vec<((i64, &'static str), f64)> = Vec::new();
        for &(h, h_total) in &self.hour_totals {
            let empty = HashMap::new();
            let o_per = obs_shares.get(&h).unwrap_or(&empty);
            let mut raw: Vec<(&'static str, f64)> = Vec::new();
            let mut refused = false;
            for &b in PARENT_COUNT_BIN_NAMES {
                let o = o_per.get(b).copied().unwrap_or(0.0);
                let g = self.gen_shares.get(&(h, b)).copied().unwrap_or(0.0);
                if o > 0.0 && g > 0.0 {
                    raw.push((b, o / g));
                } else if o == 0.0 && g > 0.0 {
                    raw.push((b, 0.0));
                } else if o > 0.0 && g == 0.0 {
                    refused = true;
                }
            }
            if refused {
                refused_hours.insert(h);
                continue;
            }
            let wsum = crate::kernel::py_sum(raw.iter().map(|(b, w)| {
                #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
                let t = self.group_total(h, b) as f64;
                w * t
            }));
            if wsum <= 0.0 {
                refused_hours.insert(h);
                continue;
            }
            #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
            let scale = h_total as f64 / wsum;
            for (b, w) in raw {
                weights.push(((h, b), w * scale));
            }
        }
        let live: Vec<(usize, f64)> = weights
            .iter()
            .filter(|((h, _), _)| !refused_hours.contains(h))
            .filter_map(|(g, w)| self.slot_of(*g).map(|i| (i, *w)))
            .collect();
        #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
        let total_w = crate::kernel::py_sum(live.iter().map(|(i, w)| w * self.groups[*i].3 as f64));
        if total_w <= 0.0 || self.support.is_empty() {
            return (None, None, refused_hours.into_iter().collect());
        }
        #[expect(clippy::cast_precision_loss, reason = "see above")]
        let exceed_w =
            crate::kernel::py_sum(live.iter().map(|(i, w)| w * self.groups[*i].4 as f64));
        let target = 0.999 * total_w;
        let (mut lo, mut hi) = (0usize, self.support.len() - 1);
        while lo < hi {
            let mid = (lo + hi) / 2;
            #[expect(clippy::cast_precision_loss, reason = "see above")]
            let cum =
                crate::kernel::py_sum(live.iter().map(|(i, w)| w * self.groups[*i].2[mid] as f64));
            if cum >= target {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        (
            Some(self.support[lo]),
            Some(exceed_w / total_w),
            refused_hours.into_iter().collect(),
        )
    }

    fn slot_of(&self, g: (i64, &'static str)) -> Option<usize> {
        self.slot.get(&g).copied()
    }

    fn group_total(&self, hour: i64, bin: &'static str) -> i64 {
        self.slot.get(&(hour, bin)).map_or(0, |i| self.groups[*i].3)
    }
}

/// Rung 2b's closure statistics: per-seed closures of the pooled
/// minute-range p99.9 gap under the count substitution, the 8-seed median
/// closure per replicate, and the nearest-rank p5 lower confidence bound.
#[derive(Clone, Debug)]
pub struct CountSubClosures {
    pub per_seed_closure: Vec<Option<f64>>,
    pub closure_median: Option<f64>,
    pub closure_lcb: Option<f64>,
    pub diagnostic_closure_to_bound: Vec<Option<f64>>,
}

/// `count_substitution_closures`.
///
/// Two strictnesses are frozen here and both fail closed: a support refusal
/// nulls that seed's closure (never a closure over partial support), and any
/// refused seed nulls the aggregate (never a median over fewer seeds). The
/// LCB likewise exists only when every replicate produced a value.
#[must_use]
pub fn count_substitution_closures(
    obs: &ObsContext,
    gen_hists: &[PooledHist],
    mults: &[Vec<i64>],
) -> CountSubClosures {
    let evals: Vec<CountSubEval> = gen_hists.iter().map(CountSubEval::new).collect();
    let t_gens: Vec<Option<i64>> = gen_hists
        .iter()
        .map(|h| {
            let mut pairs: Vec<(i64, f64)> = h
                .iter()
                .map(|(k, c)| {
                    #[expect(clippy::cast_precision_loss, reason = "minute counts stay small")]
                    let w = c as f64;
                    (k.2, w)
                })
                .collect();
            weighted_nearest_rank_fw(&mut pairs, 0.999)
        })
        .collect();
    let ones = obs.ones();

    let seed_closures = |mult: &[i64]| -> Vec<Option<f64>> {
        let shares = obs_shares_under(obs, mult);
        let t_obs = super::family::stat_minute_p999(obs, mult);
        evals
            .iter()
            .zip(&t_gens)
            .map(|(ev, t_gen)| {
                let (cf, _ex, refused) = ev.counterfactual(&shares);
                if refused.is_empty() {
                    gap_closure(int_f(*t_gen), int_f(cf), t_obs, true)
                } else {
                    None
                }
            })
            .collect()
    };

    let point_by_seed = seed_closures(&ones);
    let point = point_by_seed
        .iter()
        .all(Option::is_some)
        .then(|| median_or_none(&point_by_seed))
        .flatten();
    let mut reps: Vec<f64> = Vec::with_capacity(mults.len());
    for mult in mults {
        let seed_vals = seed_closures(mult);
        if seed_vals.iter().all(Option::is_some) {
            reps.push(median_or_none(&seed_vals).expect("a full seed set has a median"));
        }
    }
    reps.sort_by(f64::total_cmp);
    let lcb = (reps.len() == mults.len())
        .then(|| nearest_rank_p(&reps, 0.05))
        .flatten();
    let ones_shares = obs_shares_under(obs, &ones);
    let diagnostics = evals
        .iter()
        .zip(&t_gens)
        .map(|(ev, t_gen)| {
            let cf = ev.counterfactual(&ones_shares).0;
            gap_closure(int_f(*t_gen), int_f(cf), Some(399.0), true)
        })
        .collect();
    CountSubClosures {
        per_seed_closure: point_by_seed,
        closure_median: point,
        closure_lcb: lcb,
        diagnostic_closure_to_bound: diagnostics,
    }
}

#[expect(clippy::cast_precision_loss, reason = "tick ranges stay small")]
fn int_f(v: Option<i64>) -> Option<f64> {
    v.map(|x| x as f64)
}
