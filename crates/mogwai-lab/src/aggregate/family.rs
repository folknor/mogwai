// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The 6.1/6.4 metric framework: the statistic closures, the Q1 all-session
//! qualification helpers, the family inventories and the simultaneous
//! envelope with its Amendment-D refusal-ownership rules.
//!
//! Ported from `analysis/mnq_fit.py`'s `stat_*` closures, `q1_*` helpers,
//! `conditional_adequacy_bins`, `build_family_metrics` and
//! `evaluate_family`.
//!
//! ## Amendment D, stated once
//!
//! A family's simultaneous critical value exists only when EVERY metric in
//! its inventory is computable. When one is not:
//!
//! - the refused metric records a `refused: true` row with every value field
//!   null - it is never dropped from the inventory, because a disappearing
//!   metric is indistinguishable from a metric that was never required;
//! - exactly ONE additional `RefusalRec` named `envelope` owns the
//!   envelope-only nulls;
//! - every OTHER metric in that family keeps its point estimate, its
//!   bootstrap SE, its band, its point-only predicate and its seed and fold
//!   evidence, and nulls only `interval_low` / `interval_high` and the two
//!   envelope-dependent booleans. Evidence that was measured is not thrown
//!   away because a sibling could not be measured.
//!
//! Each refused metric aggregates EVERY cause into exactly one `RefusalRec`
//! (spec section 10 ownership), reasons joined with `"; "` in the frozen
//! order: `force_refused`, then the deterministic Q1 qualification lines,
//! then point-input failure, then bootstrap failure, then SE failure.

use std::rc::Rc;

use serde_json::Value;

use super::context::{Labels, ObsContext};
use super::{RefusalRec, jbool, jint, jnum, stdev_ddof1};
use crate::kernel::{median_or_none, nearest_rank_p};
use crate::session::expected_scheduled_windows;
use crate::subcontract::{
    FAIL_HOURS_300, FAMILY_ENVELOPE_LEVEL, HOT_HOURS, MATERIALITY_BAND, MIN_BOUNDARY_MINUTES_CELL,
    MIN_MINUTES_CELL, PARENT_COUNT_BIN_NAMES, SEED_DIRECTION_MIN,
};

/// The materiality band in LOG space - every `log_ratio` metric's band.
#[must_use]
pub fn log_band() -> (f64, f64) {
    (MATERIALITY_BAND.0.ln(), MATERIALITY_BAND.1.ln())
}

/// The two boundary cases, each with its boundary label pair and the
/// interior comparator it is judged against.
pub const BOUNDARY_CELLS: [(&str, Labels, Labels); 2] = [
    ("pre_halt_close", ("1800+", "0-300"), ("1800+", "300-1800")),
    (
        "post_halt_reopen",
        ("0-300", "300-1800"),
        ("300-1800", "300-1800"),
    ),
];
/// The interior comparator the localization ratio divides by.
pub const INTERIOR_LABELS: Labels = ("300-1800", "300-1800");
/// The frozen closure cells: `(hour, horizon)`.
pub const CLOSURE_CELLS: [(i64, i64); 3] = [(19, 300), (20, 300), (20, 60)];

/// A statistic: one function of a context and a multiplicity vector, shared
/// VERBATIM between the observed side (resampled) and each generated seed
/// (all-ones), so the two sides cannot drift.
pub type StatFn = Rc<dyn Fn(&ObsContext, &[i64]) -> Option<f64>>;

// -- The stat_* closures ----------------------------------------------------

/// `stat_print_excess`: the trade-range p99 over the quote-range p99 IN
/// TICKS - the quote support is in half-ticks, so it is halved before the
/// division, never after.
#[must_use]
pub fn stat_print_excess(hour: i64) -> StatFn {
    Rc::new(move |ctx, mult| {
        let tr = ctx
            .b1_support("trade", Some(hour), None, None)
            .quantile(0.99, mult)?;
        let qr = ctx
            .b1_support("quote", Some(hour), None, None)
            .quantile(0.99, mult)?;
        (qr != 0.0).then(|| tr / (qr / 2.0))
    })
}

#[must_use]
pub fn stat_robust(hour: i64, h: i64) -> StatFn {
    Rc::new(move |ctx, mult| {
        super::bootstrap::weighted_median_votes(&ctx.b3_votes(hour, h, "robust"), mult)
    })
}

#[must_use]
pub fn stat_covnorm(hour: i64) -> StatFn {
    Rc::new(move |ctx, mult| {
        super::bootstrap::weighted_median_votes(&ctx.b3_cov_votes(hour, "60-300"), mult)
    })
}

#[must_use]
pub fn stat_fano(hour: i64) -> StatFn {
    Rc::new(move |ctx, mult| ctx.b2_fano(hour, 60, mult))
}

#[must_use]
pub fn stat_count_p99(hour: i64) -> StatFn {
    Rc::new(move |ctx, mult| ctx.b2_count_quantile(hour, 60, 0.99, mult))
}

#[must_use]
pub fn stat_tail_ratio(hour_key: String) -> StatFn {
    Rc::new(move |ctx, mult| {
        super::bootstrap::weighted_median_votes(&ctx.b4_votes(&hour_key, "ratio_p999_p99"), mult)
    })
}

#[must_use]
pub fn stat_cond_sqrtn(hour: i64, bin_name: String) -> StatFn {
    Rc::new(move |ctx, mult| {
        ctx.b1_support("sqrtn", Some(hour), None, Some(&bin_name))
            .quantile(0.99, mult)
    })
}

#[must_use]
pub fn stat_boundary_quote(labels: Labels) -> StatFn {
    Rc::new(move |ctx, mult| {
        ctx.b1_support("quote", None, Some(labels), None)
            .quantile(0.99, mult)
    })
}

#[must_use]
pub fn stat_boundary_robust(labels: Labels) -> StatFn {
    let key = format!("{}|{}", labels.0, labels.1);
    Rc::new(move |ctx, mult| {
        super::bootstrap::weighted_median_votes(&ctx.b3_boundary_votes(&key, 60, "robust"), mult)
    })
}

/// `stat_boundary_excess`: the label-filtered print-excess ratio the
/// child-walk localization flag consumes.
#[must_use]
pub fn stat_boundary_excess(labels: Labels) -> StatFn {
    Rc::new(move |ctx, mult| {
        let tr = ctx
            .b1_support("trade", None, Some(labels), None)
            .quantile(0.99, mult)?;
        let qr = ctx
            .b1_support("quote", None, Some(labels), None)
            .quantile(0.99, mult)?;
        (qr != 0.0).then(|| tr / (qr / 2.0))
    })
}

/// `stat_minute_p999`: the pooled full-month minute-range p99.9.
#[must_use]
pub fn stat_minute_p999(ctx: &ObsContext, mult: &[i64]) -> Option<f64> {
    ctx.b1_support("trade", None, None, None)
        .quantile(0.999, mult)
}

// -- Q1 qualification -------------------------------------------------------

/// The observed context plus every generated seed, named as the refusal
/// strings name them.
#[must_use]
pub fn everyone<'a>(obs: &'a ObsContext, seeds: &'a [ObsContext]) -> Vec<(String, &'a ObsContext)> {
    let mut out = vec![("observed".to_string(), obs)];
    for (i, g) in seeds.iter().enumerate() {
        out.push((format!("seed {}", i + 1), g));
    }
    out
}

/// Q1 for vote-based metrics: EVERY session of EVERY context must qualify;
/// each failure is one refusal string.
#[must_use]
pub fn q1_vote_refusals(
    named: &[(String, &ObsContext)],
    votes_fn: &dyn Fn(&ObsContext) -> Rc<Vec<Option<f64>>>,
    cell: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for (who, ctx) in named {
        for (i, v) in votes_fn(ctx).iter().enumerate() {
            if v.is_none() {
                out.push(format!(
                    "{who} session {} below floor at {cell}",
                    ctx.sessions[i]
                ));
            }
        }
    }
    out
}

/// Q1 for count-floor metrics.
#[must_use]
pub fn q1_floor_refusals(
    named: &[(String, &ObsContext)],
    counts_fn: &dyn Fn(&ObsContext) -> Vec<Option<i64>>,
    floor: i64,
    cell: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for (who, ctx) in named {
        for (i, c) in counts_fn(ctx).into_iter().enumerate() {
            if c.is_none_or(|c| c < floor) {
                out.push(format!(
                    "{who} session {} carries {} below floor {floor} at {cell}",
                    ctx.sessions[i],
                    py_opt_int(c),
                ));
            }
        }
    }
    out
}

/// Q1 for count windows: scheduled-EXPOSURE completeness. Every session must
/// carry exactly the scheduled count its own CALENDAR expects - never a max
/// over the candidate's own sessions, which would be self-referential. A
/// missing serialized cell counts as zero; an expected zero with a scheduled
/// zero passes.
#[must_use]
pub fn q1_exposure_refusals(named: &[(String, &ObsContext)], hour: i64, w: i64) -> Vec<String> {
    let mut out = Vec::new();
    for (who, ctx) in named {
        for (i, s) in ctx.b2_scheduled(hour, w).into_iter().enumerate() {
            let expected = expected_scheduled_windows(&ctx.sessions[i], hour, w);
            if s.unwrap_or(0) != expected {
                out.push(format!(
                    "{who} session {} schedules {} of {expected} calendar windows at hour {hour} w {w}",
                    ctx.sessions[i],
                    py_opt_int(s),
                ));
            }
        }
    }
    out
}

/// `repr` of an optional integer the way the Python interpolates it.
fn py_opt_int(v: Option<i64>) -> String {
    v.map_or_else(|| "None".to_string(), |x| x.to_string())
}

/// One rung-2c conditional-adequacy bin: `(hour, bin, required, supported)`.
#[derive(Clone, Debug)]
pub struct CondBin {
    pub hour: i64,
    pub bin_name: String,
    pub required: bool,
    pub supported: bool,
}

/// `conditional_adequacy_bins` (spec 5.2): per implicated hour a bin is
/// REQUIRED when its pooled OBSERVED minute count reaches the floor;
/// required generated support means EVERY seed's count reaches it too. Bin
/// `"0"` is skipped - `sqrt(N)` is undefined there.
#[must_use]
pub fn conditional_adequacy_bins(obs: &ObsContext, seeds: &[ObsContext]) -> Vec<CondBin> {
    let ones = obs.ones();
    let mut out = Vec::new();
    for &hour in FAIL_HOURS_300 {
        for &bin_name in PARENT_COUNT_BIN_NAMES {
            if bin_name == "0" {
                continue;
            }
            let required = obs.b1_bin_count(hour, bin_name, &ones) >= MIN_MINUTES_CELL;
            let supported = seeds
                .iter()
                .all(|g| g.b1_bin_count(hour, bin_name, &g.ones()) >= MIN_MINUTES_CELL);
            out.push(CondBin {
                hour,
                bin_name: bin_name.to_string(),
                required,
                supported,
            });
        }
    }
    out
}

// -- Metric definitions and the family envelope -----------------------------

/// `log_ratio` compares `log(generated / observed)` against the log
/// materiality band; `raw_diff` compares `generated - observed` against
/// zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    LogRatio,
    RawDiff,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Kind::LogRatio => "log_ratio",
            Kind::RawDiff => "raw_diff",
        }
    }
}

/// `outside` fires when the point lies outside the band, `inside` when the
/// whole interval lies inside it, `raw_direction` on the sign of a raw
/// difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Predicate {
    Outside,
    Inside,
    RawDirection,
}

impl Predicate {
    const fn as_str(self) -> &'static str {
        match self {
            Predicate::Outside => "outside",
            Predicate::Inside => "inside",
            Predicate::RawDirection => "raw_direction",
        }
    }
}

pub struct MetricDef {
    pub name: String,
    pub kind: Kind,
    pub predicate: Predicate,
    pub stat: StatFn,
    pub gen_seeds: Vec<Option<f64>>,
    pub gen_central: Option<f64>,
    pub qualify_refusals: Vec<String>,
    pub force_refused: Option<String>,
}

/// One serialized `MetricRec` (spec section 10). Every field is nullable
/// except `name`, `kind`, `predicate` and `refused`.
#[derive(Clone, Debug, Default)]
pub struct MetricRec {
    pub name: String,
    pub kind: &'static str,
    pub predicate: &'static str,
    pub point: Option<f64>,
    pub se: Option<f64>,
    pub interval_low: Option<f64>,
    pub interval_high: Option<f64>,
    pub band_low: Option<f64>,
    pub band_high: Option<f64>,
    pub outside_band: Option<bool>,
    pub envelope_excludes_edge: Option<bool>,
    pub interval_inside_band: Option<bool>,
    pub seed_same_side_count: Option<i64>,
    pub seed_inside_count: Option<i64>,
    pub seed_rule_pass: Option<bool>,
    pub fold_rule_pass: Option<bool>,
    pub refused: bool,
}

impl MetricRec {
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "kind": self.kind,
            "predicate": self.predicate,
            "point": jnum(self.point),
            "se": jnum(self.se),
            "interval_low": jnum(self.interval_low),
            "interval_high": jnum(self.interval_high),
            "band_low": jnum(self.band_low),
            "band_high": jnum(self.band_high),
            "outside_band": jbool(self.outside_band),
            "envelope_excludes_edge": jbool(self.envelope_excludes_edge),
            "interval_inside_band": jbool(self.interval_inside_band),
            "seed_same_side_count": jint(self.seed_same_side_count),
            "seed_inside_count": jint(self.seed_inside_count),
            "seed_rule_pass": jbool(self.seed_rule_pass),
            "fold_rule_pass": jbool(self.fold_rule_pass),
            "refused": self.refused,
        })
    }
}

/// One family's evaluated envelope.
#[derive(Clone, Debug)]
pub struct FamilyEnvelope {
    pub metrics: Vec<MetricRec>,
    pub critical_value: Option<f64>,
    pub inventory_complete: bool,
    pub refusals: Vec<RefusalRec>,
}

impl FamilyEnvelope {
    /// The metric by name. A miss is a programming error - the ladder only
    /// asks for metrics its own inventory defined.
    #[must_use]
    pub fn metric(&self, name: &str) -> &MetricRec {
        self.metrics
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("{name} is missing from the envelope"))
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "metrics": self.metrics.iter().map(MetricRec::to_json).collect::<Vec<_>>(),
            "critical_value": jnum(self.critical_value),
            "inventory_complete": self.inventory_complete,
        })
    }
}

struct Prepared<'a> {
    def: &'a MetricDef,
    t_obs: Option<f64>,
    point: Option<f64>,
    reps: Option<Vec<Option<f64>>>,
    se: Option<f64>,
    refused: bool,
}

/// `evaluate_family`: one family's simultaneous envelope.
#[must_use]
pub fn evaluate_family(
    family: &str,
    metrics: &[MetricDef],
    obs: &ObsContext,
    mults: &[Vec<i64>],
    folds: &[Vec<i64>],
    ones: &[i64],
) -> FamilyEnvelope {
    let mut refusal_recs: Vec<RefusalRec> = Vec::new();
    let mut prepared: Vec<Prepared<'_>> = Vec::with_capacity(metrics.len());

    for m in metrics {
        let mut reasons: Vec<String> = Vec::new();
        let mut t_obs = None;
        let mut point = None;
        let mut reps: Option<Vec<Option<f64>>> = None;
        let mut se = None;
        if let Some(forced) = &m.force_refused {
            reasons.push(forced.clone());
        }
        reasons.extend(m.qualify_refusals.iter().cloned());
        if reasons.is_empty() {
            t_obs = (m.stat)(obs, ones);
            let g = m.gen_central;
            match m.kind {
                Kind::LogRatio => match (t_obs, g) {
                    (Some(t), Some(g)) if t > 0.0 && g > 0.0 => point = Some((g / t).ln()),
                    _ => reasons.push("nonpositive or missing point inputs".to_string()),
                },
                Kind::RawDiff => match (t_obs, g) {
                    (Some(t), Some(g)) => point = Some(g - t),
                    _ => reasons.push("missing point inputs".to_string()),
                },
            }
        }
        if reasons.is_empty() {
            let g = m.gen_central.expect("the point check passed");
            let built: Vec<Option<f64>> = mults
                .iter()
                .map(|mult| {
                    let tb = (m.stat)(obs, mult);
                    match m.kind {
                        Kind::LogRatio => tb.filter(|t| *t > 0.0).map(|t| (g / t).ln()),
                        Kind::RawDiff => tb.map(|t| g - t),
                    }
                })
                .collect();
            // A missing or non-finite replicate REFUSES the metric - never a
            // silent omission from the SE population.
            if built.iter().any(|r| !r.is_some_and(f64::is_finite)) {
                reasons.push("missing or non-finite bootstrap replicate".to_string());
            } else {
                se = stdev_ddof1(&built);
                if !se.is_some_and(|s| s != 0.0 && s.is_finite()) {
                    reasons.push("zero or non-finite bootstrap SE".to_string());
                    se = None;
                }
            }
            reps = Some(built);
        }
        let refused = !reasons.is_empty();
        if refused {
            refusal_recs.push(RefusalRec::new(
                format!("family:{family}"),
                m.name.clone(),
                reasons.join("; "),
            ));
        }
        prepared.push(Prepared {
            def: m,
            t_obs,
            point,
            reps,
            se,
            refused,
        });
    }

    let mut inventory_complete = prepared.iter().all(|p| !p.refused);
    let mut critical = None;
    if inventory_complete && !prepared.is_empty() {
        let mut maxima: Vec<f64> = (0..mults.len())
            .map(|i| {
                prepared
                    .iter()
                    .map(|p| {
                        let rep = p.reps.as_ref().expect("a complete metric has replicates")[i]
                            .expect("a complete metric has finite replicates");
                        (rep - p.point.expect("a complete metric has a point")).abs()
                            / p.se.expect("a complete metric has an SE")
                    })
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect();
        maxima.sort_by(f64::total_cmp);
        critical = nearest_rank_p(&maxima, FAMILY_ENVELOPE_LEVEL);
        if !critical.is_some_and(f64::is_finite) {
            inventory_complete = false;
            critical = None;
        }
    }
    if !inventory_complete {
        // Exactly ONE family-envelope refusal owns the envelope-only nulls
        // on the otherwise computable metrics (Amendment D).
        refusal_recs.push(RefusalRec::new(
            format!("family:{family}"),
            "envelope",
            "incomplete metric inventory - no simultaneous critical value",
        ));
    }

    let band = log_band();
    let mut records = Vec::with_capacity(prepared.len());
    for p in &prepared {
        let mut rec = MetricRec {
            name: p.def.name.clone(),
            kind: p.def.kind.as_str(),
            predicate: p.def.predicate.as_str(),
            refused: p.refused,
            ..MetricRec::default()
        };
        if p.refused {
            records.push(rec);
            continue;
        }
        let point = p.point.expect("a computable metric has a point");
        let se = p.se.expect("a computable metric has an SE");
        let (lo, hi) = match critical {
            Some(c) => {
                let half = c * se;
                (Some(point - half), Some(point + half))
            }
            // Computable metric in an INCOMPLETE family: point, SE, band,
            // point-only predicate, seed and fold evidence all stay; only
            // the envelope fields go null (Amendment D).
            None => (None, None),
        };
        rec.point = Some(point);
        rec.se = Some(se);
        rec.interval_low = lo;
        rec.interval_high = hi;

        let t_obs = p.t_obs;
        let seed_points: Vec<f64> = p
            .def
            .gen_seeds
            .iter()
            .filter_map(|s| *s)
            .filter_map(|s| match p.def.kind {
                Kind::LogRatio => match t_obs {
                    Some(t) if s > 0.0 && t != 0.0 && t > 0.0 => Some((s / t).ln()),
                    _ => None,
                },
                Kind::RawDiff => t_obs.map(|t| s - t),
            })
            .collect();
        let fold_points = || -> Vec<Option<f64>> {
            folds
                .iter()
                .map(|f| {
                    let tf = (p.def.stat)(obs, f);
                    match p.def.kind {
                        Kind::LogRatio => tf.filter(|t| *t > 0.0).map(|t| {
                            (p.def
                                .gen_central
                                .expect("a computable metric has a central")
                                / t)
                                .ln()
                        }),
                        Kind::RawDiff => tf.map(|t| {
                            p.def
                                .gen_central
                                .expect("a computable metric has a central")
                                - t
                        }),
                    }
                })
                .collect()
        };

        match p.def.predicate {
            Predicate::Outside => {
                rec.band_low = Some(band.0);
                rec.band_high = Some(band.1);
                let below = point < band.0;
                let above = point > band.1;
                rec.outside_band = Some(below || above);
                rec.envelope_excludes_edge = lo.map(|lo| {
                    let hi = hi.expect("both interval ends exist together");
                    (below && hi < band.0) || (above && lo > band.1)
                });
                let same_side = seed_points
                    .iter()
                    .filter(|s| (**s < band.0 && below) || (**s > band.1 && above))
                    .count() as i64;
                rec.seed_same_side_count = Some(same_side);
                rec.seed_rule_pass = Some(same_side >= SEED_DIRECTION_MIN);
                let fp = fold_points();
                rec.fold_rule_pass = Some(
                    !fp.is_empty()
                        && fp.iter().all(|v| {
                            v.is_some_and(|v| (below && v < band.0) || (above && v > band.1))
                        }),
                );
            }
            Predicate::Inside => {
                rec.band_low = Some(band.0);
                rec.band_high = Some(band.1);
                let inside = band.0 <= point && point <= band.1;
                rec.interval_inside_band = lo.map(|lo| {
                    let hi = hi.expect("both interval ends exist together");
                    inside && band.0 <= lo && hi <= band.1
                });
                let cnt = seed_points
                    .iter()
                    .filter(|s| band.0 <= **s && **s <= band.1)
                    .count() as i64;
                rec.seed_inside_count = Some(cnt);
                rec.seed_rule_pass = Some(cnt >= SEED_DIRECTION_MIN);
                let fp = fold_points();
                rec.fold_rule_pass = Some(
                    !fp.is_empty()
                        && fp
                            .iter()
                            .all(|v| v.is_some_and(|v| band.0 <= v && v <= band.1)),
                );
            }
            Predicate::RawDirection => {
                let claimed = if point > 0.0 {
                    1
                } else if point < 0.0 {
                    -1
                } else {
                    0
                };
                rec.outside_band = Some(claimed != 0);
                rec.envelope_excludes_edge = lo.map(|lo| {
                    let hi = hi.expect("both interval ends exist together");
                    (claimed > 0 && lo > 0.0) || (claimed < 0 && hi < 0.0)
                });
                let same = seed_points
                    .iter()
                    .filter(|s| (claimed > 0 && **s > 0.0) || (claimed < 0 && **s < 0.0))
                    .count() as i64;
                rec.seed_same_side_count = Some(same);
                rec.seed_rule_pass = Some(same >= SEED_DIRECTION_MIN);
                let fp = fold_points();
                rec.fold_rule_pass = Some(
                    !fp.is_empty()
                        && fp.iter().all(|v| {
                            v.is_some_and(|v| (claimed > 0 && v > 0.0) || (claimed < 0 && v < 0.0))
                        }),
                );
            }
        }
        records.push(rec);
    }

    FamilyEnvelope {
        metrics: records,
        critical_value: critical,
        inventory_complete,
        refusals: refusal_recs,
    }
}

// -- The 6.4 inventories ----------------------------------------------------

/// The six families in the frozen ladder order, plus the conditional bins
/// the arrival rung consumes.
pub struct FamilyInventories {
    pub families: Vec<(&'static str, Vec<MetricDef>)>,
    pub cond_bins: Vec<CondBin>,
}

/// `build_family_metrics`: the 6.4 inventories with the observed evaluators
/// bound to `obs`, the generated values taken from the per-seed contexts at
/// all-ones, and the Q1 all-session qualification refusals attached per
/// metric.
#[must_use]
pub fn build_family_metrics(obs: &ObsContext, seeds: &[ObsContext]) -> FamilyInventories {
    let named = everyone(obs, seeds);

    let defn = |name: String,
                kind: Kind,
                predicate: Predicate,
                stat: StatFn,
                qualify: Vec<String>,
                force_refused: Option<String>| {
        let gen_seeds: Vec<Option<f64>> = seeds.iter().map(|g| stat(g, &g.ones())).collect();
        let gen_central = median_or_none(&gen_seeds);
        MetricDef {
            name,
            kind,
            predicate,
            stat,
            gen_seeds,
            gen_central,
            qualify_refusals: qualify,
            force_refused,
        }
    };

    let q_minutes = |hour: i64| {
        q1_floor_refusals(
            &named,
            &move |ctx: &ObsContext| {
                ctx.minute_counts(Some(hour), None)
                    .iter()
                    .map(|c| Some(*c))
                    .collect()
            },
            MIN_MINUTES_CELL,
            &format!("hour {hour} minutes"),
        )
    };
    let q_labels = |labels: Labels| {
        q1_floor_refusals(
            &named,
            &move |ctx: &ObsContext| {
                ctx.minute_counts(None, Some(labels))
                    .iter()
                    .map(|c| Some(*c))
                    .collect()
            },
            MIN_BOUNDARY_MINUTES_CELL,
            &format!("boundary minutes {}|{}", labels.0, labels.1),
        )
    };

    // -- child_walk
    let mut child_walk = Vec::new();
    for &h in FAIL_HOURS_300 {
        child_walk.push(defn(
            format!("print_excess_h{h}"),
            Kind::LogRatio,
            Predicate::Outside,
            stat_print_excess(h),
            q_minutes(h),
            None,
        ));
    }
    for &h in FAIL_HOURS_300 {
        for w in [60i64, 300] {
            child_walk.push(defn(
                format!("quote_robust_{w}_h{h}"),
                Kind::LogRatio,
                Predicate::Inside,
                stat_robust(h, w),
                q1_vote_refusals(
                    &named,
                    &move |ctx: &ObsContext| ctx.b3_votes(h, w, "robust"),
                    &format!("robust {w} hour {h}"),
                ),
                None,
            ));
        }
    }

    // -- arrival
    let mut arrival = Vec::new();
    for &h in FAIL_HOURS_300 {
        arrival.push(defn(
            format!("fano_60_h{h}"),
            Kind::LogRatio,
            Predicate::Outside,
            stat_fano(h),
            q1_exposure_refusals(&named, h, 60),
            None,
        ));
    }
    for &h in FAIL_HOURS_300 {
        arrival.push(defn(
            format!("count_p99_60_h{h}"),
            Kind::LogRatio,
            Predicate::Outside,
            stat_count_p99(h),
            q1_exposure_refusals(&named, h, 60),
            None,
        ));
    }
    let cond_bins = conditional_adequacy_bins(obs, seeds);
    for cb in &cond_bins {
        if !cb.required {
            continue;
        }
        // A required-but-unsupported conditional metric stays PRESENT as a
        // refused record (Amendment D), never omitted from the inventory.
        let forced = (!cb.supported)
            .then(|| "required observed bin without required generated support".to_string());
        arrival.push(defn(
            format!("cond_sqrtn_p99_h{}_{}", cb.hour, cb.bin_name),
            Kind::LogRatio,
            Predicate::Inside,
            stat_cond_sqrtn(cb.hour, cb.bin_name.clone()),
            Vec::new(),
            forced,
        ));
    }

    // -- innovation
    let mut innovation = Vec::new();
    for &h in FAIL_HOURS_300 {
        innovation.push(defn(
            format!("tail_ratio_h{h}"),
            Kind::LogRatio,
            Predicate::Outside,
            stat_tail_ratio(h.to_string()),
            q1_vote_refusals(
                &named,
                &move |ctx: &ObsContext| ctx.b4_votes(&h.to_string(), "ratio_p999_p99"),
                &format!("residuals hour {h}"),
            ),
            None,
        ));
    }
    innovation.push(defn(
        "tail_ratio_all".to_string(),
        Kind::LogRatio,
        Predicate::Outside,
        stat_tail_ratio("all".to_string()),
        q1_vote_refusals(
            &named,
            &|ctx: &ObsContext| ctx.b4_votes("all", "ratio_p999_p99"),
            "residuals all-hours",
        ),
        None,
    ));

    // -- reversion
    let mut reversion = Vec::new();
    for &h in HOT_HOURS {
        reversion.push(defn(
            format!("robust_300_h{h}"),
            Kind::LogRatio,
            Predicate::Outside,
            stat_robust(h, 300),
            q1_vote_refusals(
                &named,
                &move |ctx: &ObsContext| ctx.b3_votes(h, 300, "robust"),
                &format!("robust 300 hour {h}"),
            ),
            None,
        ));
    }
    reversion.push(defn(
        "robust_60_h20".to_string(),
        Kind::LogRatio,
        Predicate::Outside,
        stat_robust(20, 60),
        q1_vote_refusals(
            &named,
            &|ctx: &ObsContext| ctx.b3_votes(20, 60, "robust"),
            "robust 60 hour 20",
        ),
        None,
    ));
    for &h in HOT_HOURS {
        reversion.push(defn(
            format!("covnorm_h{h}"),
            Kind::RawDiff,
            Predicate::RawDirection,
            stat_covnorm(h),
            q1_vote_refusals(
                &named,
                &move |ctx: &ObsContext| ctx.b3_cov_votes(h, "60-300"),
                &format!("covnorm hour {h}"),
            ),
            None,
        ));
    }

    // -- garch (the same three scale metrics the reversion rung consumes;
    // the two families are evaluated SEPARATELY so one rung's incompleteness
    // cannot silently widen the other's envelope).
    let mut garch = Vec::new();
    for h in [19i64, 20] {
        garch.push(defn(
            format!("robust_300_h{h}"),
            Kind::LogRatio,
            Predicate::Outside,
            stat_robust(h, 300),
            q1_vote_refusals(
                &named,
                &move |ctx: &ObsContext| ctx.b3_votes(h, 300, "robust"),
                &format!("robust 300 hour {h}"),
            ),
            None,
        ));
    }
    garch.push(defn(
        "robust_60_h20".to_string(),
        Kind::LogRatio,
        Predicate::Outside,
        stat_robust(20, 60),
        q1_vote_refusals(
            &named,
            &|ctx: &ObsContext| ctx.b3_votes(20, 60, "robust"),
            "robust 60 hour 20",
        ),
        None,
    ));

    // -- boundary
    let mut boundary = Vec::new();
    for (case, b_labels, c_labels) in BOUNDARY_CELLS {
        for (labels, suffix, predicate) in [
            (b_labels, "", Predicate::Outside),
            (c_labels, "_comparator", Predicate::Inside),
        ] {
            boundary.push(defn(
                format!("quote_p99_{case}{suffix}"),
                Kind::LogRatio,
                predicate,
                stat_boundary_quote(labels),
                q_labels(labels),
                None,
            ));
            let key = format!("{}|{}", labels.0, labels.1);
            boundary.push(defn(
                format!("robust_60_{case}{suffix}"),
                Kind::LogRatio,
                predicate,
                stat_boundary_robust(labels),
                q1_vote_refusals(
                    &named,
                    &move |ctx: &ObsContext| ctx.b3_boundary_votes(&key, 60, "robust"),
                    &format!("boundary robust {}|{}", labels.0, labels.1),
                ),
                None,
            ));
        }
    }

    FamilyInventories {
        families: vec![
            ("child_walk", child_walk),
            ("arrival", arrival),
            ("innovation", innovation),
            ("reversion", reversion),
            ("garch", garch),
            ("boundary", boundary),
        ],
        cond_bins,
    }
}
