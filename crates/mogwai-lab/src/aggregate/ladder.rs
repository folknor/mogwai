// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The frozen 6.2 ladder, plus the closure and forensic statistics it
//! consumes. Ported from the retired Python fit implementation's `closure_analysis`,
//! `worsening_23_analysis`, `forensic_subchecks` and `evaluate_ladder`.
//!
//! Every rung is evaluated in order and every fired rung is recorded; the
//! selected rung is the first that fired, and a ladder with none fired
//! returns the `no-family-eligible` verdict. The ladder never short-circuits:
//! a later rung's evidence is measured and recorded even when an earlier one
//! already fired, because the record is the deliverable.
//!
//! ## Fail-closed, stated three ways
//!
//! - **Amendment D (completeness gates).** `fires_outside` and
//!   `clean_inside` are false whenever the family inventory is incomplete,
//!   whatever the individual metric says. The reversion rung's `a_closure`
//!   and the GARCH rung's `a_closure` additionally test `complete(fam)`
//!   directly, because they consume required inventory cells rather than one
//!   named metric.
//! - **Amendment E (worsening_23).** The diagnostic is evaluated only on a
//!   fired reversion rung. An unfired rung leaves it null by
//!   inapplicability, with no refusal record; a fired rung that cannot
//!   measure it records null with exactly one matching refusal. The object
//!   itself is nullable, not its members - a partially measured
//!   `worsening_23` is not a thing.
//! - **Amendment C (localization).** The boundary-localization flag is a
//!   Boolean only for a fired `child_walk` / `reversion` / `garch` rung
//!   whose inputs all qualify; otherwise it is null with a refusal.
//!
//! Each rung mirrors its own refusals plus the refusals of the families it
//! consumed, so a reader of one rung record sees every reason it could not
//! conclude without cross-referencing the envelope section.

use serde_json::Value;

use super::context::ObsContext;
use super::countsub::{CountSubClosures, count_substitution_closures, gap_closure};
use super::family::{
    BOUNDARY_CELLS, CLOSURE_CELLS, CondBin, FamilyEnvelope, INTERIOR_LABELS, MetricRec, StatFn,
    build_family_metrics, evaluate_family, stat_boundary_excess, stat_boundary_robust,
};
use super::monthly::PooledHist;
use super::{RefusalRec, jbool, jnum, stdev_ddof1};
use crate::aggregate::bootstrap::{fold_multiplicities, weighted_median_votes};
use crate::kernel::{median_or_none, nearest_rank_p};
use crate::subcontract::{
    CONTROL_ESCALATION_MAX, FAIL_HOURS_300, GAP_CLOSE_LCB_MIN, GAP_CLOSE_MIN, HOT_HOURS,
    INITIATION_INNOVATION_MIN, SEED_DIRECTION_MIN, SIGMA_ESCALATION_MIN,
};

// -- Closures ---------------------------------------------------------------

/// One closure cell's point estimate.
#[derive(Clone, Debug)]
pub struct ClosureCell {
    pub hour: i64,
    pub horizon: i64,
    pub closure: Option<f64>,
}

/// The sign or magnitude shuffle gap closures over the frozen cells.
#[derive(Clone, Debug)]
pub struct ClosureAnalysis {
    pub cells: Vec<ClosureCell>,
    pub joint_lcb: Option<f64>,
    pub all_points_pass: bool,
}

/// `closure_analysis`: per-cell point closures plus the multi-target joint
/// lower confidence bound - the per-replicate minimum across cells, then the
/// nearest-rank p5.
///
/// The 5.3 strictness is the whole point: the joint LCB exists only when
/// every bootstrap replicate produced a value across every cell. One
/// unavailable replicate refuses the bound and the consuming rung fails
/// closed - there is no partial joint statement.
#[must_use]
pub fn closure_analysis(
    obs: &ObsContext,
    seeds: &[ObsContext],
    variant: &'static str,
    mults: &[Vec<i64>],
) -> ClosureAnalysis {
    let ones = obs.ones();
    let t_gens: Vec<Option<f64>> = CLOSURE_CELLS
        .iter()
        .map(|&(hour, h)| {
            let gen_vals: Vec<Option<f64>> = seeds
                .iter()
                .map(|g| weighted_median_votes(&g.b3_votes(hour, h, "robust"), &g.ones()))
                .collect();
            median_or_none(&gen_vals)
        })
        .collect();

    let closure_at = |mult: &[i64], idx: usize| -> Option<f64> {
        let (hour, h) = CLOSURE_CELLS[idx];
        gap_closure(
            t_gens[idx],
            obs.perm_value(variant, hour, h, mult),
            obs.b3_robust_strict(hour, h, mult),
            false,
        )
    };

    let point_closures: Vec<Option<f64>> = (0..CLOSURE_CELLS.len())
        .map(|i| closure_at(&ones, i))
        .collect();
    let mut minima: Vec<f64> = Vec::with_capacity(mults.len());
    for mult in mults {
        let mut worst: Option<f64> = None;
        let mut refused = false;
        for i in 0..CLOSURE_CELLS.len() {
            match closure_at(mult, i) {
                None => {
                    refused = true;
                    break;
                }
                Some(cl) => {
                    if worst.is_none_or(|w| cl < w) {
                        worst = Some(cl);
                    }
                }
            }
        }
        if !refused && let Some(w) = worst {
            minima.push(w);
        }
    }
    minima.sort_by(f64::total_cmp);
    let joint_lcb = (minima.len() == mults.len())
        .then(|| nearest_rank_p(&minima, 0.05))
        .flatten();
    ClosureAnalysis {
        cells: CLOSURE_CELLS
            .iter()
            .zip(&point_closures)
            .map(|(&(hour, horizon), &closure)| ClosureCell {
                hour,
                horizon,
                closure,
            })
            .collect(),
        joint_lcb,
        all_points_pass: point_closures
            .iter()
            .all(|pc| pc.is_some_and(|c| c >= GAP_CLOSE_MIN)),
    }
}

/// `worsening_23_analysis`: `|log(G/P)| - |log(G/O)|` at the 300 s robust
/// scale of hour 23, with the nearest-rank p95 upper confidence bound.
///
/// The whole object refuses (returns `None`) unless the point and every
/// bootstrap replicate produced a value - section 10 makes the object
/// nullable, not its members.
#[must_use]
pub fn worsening_23_analysis(
    obs: &ObsContext,
    seeds: &[ObsContext],
    mults: &[Vec<i64>],
) -> Option<Worsening23> {
    let gen_vals: Vec<Option<f64>> = seeds
        .iter()
        .map(|g| weighted_median_votes(&g.b3_votes(23, 300, "robust"), &g.ones()))
        .collect();
    let t_gen = median_or_none(&gen_vals);
    let value = |mult: &[i64]| -> Option<f64> {
        let o = obs.b3_robust_strict(23, 300, mult)?;
        let p = obs.perm_value("sign", 23, 300, mult)?;
        let g = t_gen?;
        if g <= 0.0 || o <= 0.0 || p <= 0.0 {
            return None;
        }
        Some((g / p).ln().abs() - (g / o).ln().abs())
    };
    let point = value(&obs.ones())?;
    let mut reps: Vec<f64> = mults.iter().filter_map(|m| value(m)).collect();
    if reps.len() != mults.len() {
        return None;
    }
    reps.sort_by(f64::total_cmp);
    let as_opt: Vec<Option<f64>> = reps.iter().map(|v| Some(*v)).collect();
    Some(Worsening23 {
        point,
        se: stdev_ddof1(&as_opt),
        ucb: nearest_rank_p(&reps, 0.95),
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Worsening23 {
    pub point: f64,
    pub se: Option<f64>,
    pub ucb: Option<f64>,
}

impl Worsening23 {
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "point": self.point,
            "se": jnum(self.se),
            "ucb": jnum(self.ucb),
        })
    }
}

// -- Forensic subchecks -----------------------------------------------------

/// The trace-based rung inputs (rungs 3b, 3c, 5b) over the per-seed forensic
/// records. A seed's `control` record is the one matched to that seed's
/// extreme minute; an unmatched control contributes nothing.
#[derive(Clone, Debug, Default)]
pub struct ForensicSubchecks {
    pub initiation_seed_count: i64,
    pub initiation_control_count: i64,
    pub escalation_seed_count: i64,
    pub control_escalation_median: Option<f64>,
}

#[must_use]
pub fn forensic_subchecks(per_seed_forensic: &[&Value]) -> ForensicSubchecks {
    let mut out = ForensicSubchecks::default();
    let mut control_escs: Vec<Option<f64>> = Vec::new();
    #[expect(clippy::cast_precision_loss, reason = "the frozen bound is 8")]
    let innovation_min = INITIATION_INNOVATION_MIN as f64;
    for seed_rec in per_seed_forensic {
        let recs = super::ja(seed_rec, "records");
        let extreme = recs
            .iter()
            .find(|r| super::js(r, "kind") == "extreme_range");
        let control = recs.iter().find(|r| {
            super::js(r, "kind") == "control"
                && extreme.is_some_and(|e| {
                    r.get("matched_extreme_minute_start") == e.get("minute_start_ns")
                })
        });
        if let Some(e) = extreme {
            let initiation = e["initiation"].as_bool().unwrap_or(false);
            let largest = super::jf_opt(e, "largest_innovation_std");
            if initiation && largest.is_some_and(|v| v > innovation_min) {
                out.initiation_seed_count += 1;
            }
            if super::jf_opt(e, "sigma_escalation").is_some_and(|v| v >= SIGMA_ESCALATION_MIN) {
                out.escalation_seed_count += 1;
            }
        }
        if let Some(c) = control {
            let initiation = c["initiation"].as_bool().unwrap_or(false);
            let largest = super::jf_opt(c, "largest_innovation_std");
            if initiation && largest.is_some_and(|v| v > innovation_min) {
                out.initiation_control_count += 1;
            }
            if let Some(esc) = super::jf_opt(c, "sigma_escalation") {
                control_escs.push(Some(esc));
            }
        }
    }
    out.control_escalation_median = median_or_none(&control_escs);
    out
}

// -- The ladder -------------------------------------------------------------

/// One serialized rung record.
#[derive(Clone, Debug)]
pub struct RungRec {
    pub name: &'static str,
    /// The named subchecks, in the frozen per-rung order.
    pub subchecks: Vec<(&'static str, bool)>,
    pub fired: bool,
    pub boundary_localized: Option<bool>,
    pub uniform_eligible: Option<bool>,
    pub required_resolution: Option<&'static str>,
    pub refusals: Vec<RefusalRec>,
}

impl RungRec {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let subchecks: serde_json::Map<String, Value> = self
            .subchecks
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::Bool(*v)))
            .collect();
        serde_json::json!({
            "name": self.name,
            "subchecks": Value::Object(subchecks),
            "fired": self.fired,
            "boundary_localized": jbool(self.boundary_localized),
            "refusals": self.refusals.iter().map(RefusalRec::to_json).collect::<Vec<_>>(),
            "uniform_eligible": jbool(self.uniform_eligible),
            "required_resolution": match self.required_resolution {
                Some(s) => Value::String(s.to_string()),
                None => Value::Null,
            },
        })
    }
}

/// Everything `evaluate_ladder` produces - the ladder section itself plus
/// the pieces 2c pastes elsewhere in the artifact.
pub struct LadderOutcome {
    /// The six families in ladder order.
    pub envelopes: Vec<(&'static str, FamilyEnvelope)>,
    pub count_substitution: CountSubClosures,
    pub cond_bins: Vec<CondBin>,
    pub sign_closure: ClosureAnalysis,
    pub magnitude_closure: ClosureAnalysis,
    pub worsening_23: Option<Worsening23>,
    pub forensic_subchecks: ForensicSubchecks,
    pub rungs: Vec<RungRec>,
    pub eligible: Vec<&'static str>,
    pub selected: Option<&'static str>,
    pub verdict: &'static str,
}

impl LadderOutcome {
    /// The named family envelope.
    #[must_use]
    pub fn envelope(&self, family: &str) -> &FamilyEnvelope {
        self.envelopes
            .iter()
            .find(|(n, _)| *n == family)
            .map_or_else(|| panic!("no {family} envelope"), |(_, e)| e)
    }

    /// The `ladder` section of the artifact.
    #[must_use]
    pub fn ladder_json(&self) -> Value {
        serde_json::json!({
            "rungs": self.rungs.iter().map(RungRec::to_json).collect::<Vec<_>>(),
            "eligible": self.eligible,
            "selected": match self.selected {
                Some(s) => Value::String(s.to_string()),
                None => Value::Null,
            },
            "verdict": self.verdict,
        })
    }

    /// The `bootstrap` section of the artifact.
    #[must_use]
    pub fn bootstrap_json(&self, replicates: usize) -> Value {
        let per_family: serde_json::Map<String, Value> = self
            .envelopes
            .iter()
            .map(|(name, env)| ((*name).to_string(), env.to_json()))
            .collect();
        serde_json::json!({
            "seed_rule": "splitmix64(BOOTSTRAP_BASE_SEED xor (replicate << 8) xor block) mod sessions",
            "replicates": replicates,
            "per_family": Value::Object(per_family),
        })
    }
}

/// `evaluate_ladder`.
#[must_use]
pub fn evaluate_ladder(
    obs: &ObsContext,
    seeds: &[ObsContext],
    gen_hists: &[PooledHist],
    per_seed_forensic: &[&Value],
    mults: &[Vec<i64>],
) -> LadderOutcome {
    let folds = fold_multiplicities(&obs.sessions);
    let ones = obs.ones();
    let inventories = build_family_metrics(obs, seeds);
    let cond_bins = inventories.cond_bins;
    let envelopes: Vec<(&'static str, FamilyEnvelope)> = inventories
        .families
        .iter()
        .map(|(name, defs)| {
            (
                *name,
                evaluate_family(name, defs, obs, mults, &folds, &ones),
            )
        })
        .collect();
    let env_of = |family: &str| -> &FamilyEnvelope {
        envelopes
            .iter()
            .find(|(n, _)| *n == family)
            .map_or_else(|| panic!("no {family} envelope"), |(_, e)| e)
    };
    let metric = |family: &str, name: &str| -> &MetricRec { env_of(family).metric(name) };
    let complete = |family: &str| -> bool { env_of(family).inventory_complete };
    // Envelope-dependent: false whenever the family inventory is incomplete
    // (Amendment D), whatever the individual metric records.
    let fires_outside = |family: &str, m: &MetricRec| -> bool {
        complete(family)
            && !m.refused
            && m.outside_band.unwrap_or(false)
            && m.envelope_excludes_edge.unwrap_or(false)
            && m.seed_rule_pass.unwrap_or(false)
            && m.fold_rule_pass.unwrap_or(false)
    };
    let clean_inside = |family: &str, m: &MetricRec| -> bool {
        complete(family)
            && !m.refused
            && m.interval_inside_band.unwrap_or(false)
            && m.seed_rule_pass.unwrap_or(false)
            && m.fold_rule_pass.unwrap_or(false)
    };

    let mut rungs: Vec<RungRec> = Vec::new();
    let mut refusals: Vec<RefusalRec> = Vec::new();

    // -- Rung 1: child-walk isolation, paired by hour. The pairing matters:
    // an excess at one hour and a clean mid at another is not isolation.
    let per_hour: Vec<bool> = FAIL_HOURS_300
        .iter()
        .map(|&h| {
            let a = fires_outside(
                "child_walk",
                metric("child_walk", &format!("print_excess_h{h}")),
            );
            let b = [60i64, 300].iter().all(|w| {
                clean_inside(
                    "child_walk",
                    metric("child_walk", &format!("quote_robust_{w}_h{h}")),
                )
            });
            a && b
        })
        .collect();
    let sub1_a = FAIL_HOURS_300.iter().any(|&h| {
        fires_outside(
            "child_walk",
            metric("child_walk", &format!("print_excess_h{h}")),
        )
    });
    let sub1_b = per_hour.iter().any(|v| *v);
    rungs.push(RungRec {
        name: "child_walk",
        subchecks: vec![("a_print_excess", sub1_a), ("b_mid_clean", sub1_b)],
        fired: sub1_b,
        boundary_localized: None,
        uniform_eligible: None,
        required_resolution: None,
        refusals: Vec::new(),
    });

    // -- Rung 2: arrival sufficiency.
    let a_env = FAIL_HOURS_300.iter().any(|&h| {
        ["fano_60", "count_p99_60"]
            .iter()
            .any(|stat| fires_outside("arrival", metric("arrival", &format!("{stat}_h{h}"))))
    });
    let csub = count_substitution_closures(obs, gen_hists, mults);
    let b_closure = csub.closure_median.is_some_and(|m| m >= GAP_CLOSE_MIN)
        && csub.closure_lcb.is_some_and(|l| l > GAP_CLOSE_LCB_MIN);
    // An unsupported required bin is a force_refused metric: clean_inside is
    // false and the family inventory is incomplete, so the rung fails closed
    // with the refusal mirrored from the family envelope.
    let cond_ok = cond_bins.iter().filter(|cb| cb.required).all(|cb| {
        clean_inside(
            "arrival",
            metric(
                "arrival",
                &format!("cond_sqrtn_p99_h{}_{}", cb.hour, cb.bin_name),
            ),
        )
    });
    rungs.push(RungRec {
        name: "arrival",
        subchecks: vec![
            ("a_envelope", a_env),
            ("b_closure", b_closure),
            ("c_conditional", cond_ok),
        ],
        fired: a_env && b_closure && cond_ok,
        boundary_localized: None,
        uniform_eligible: None,
        required_resolution: None,
        refusals: Vec::new(),
    });

    // -- Rung 3: innovation tail.
    let forensics = forensic_subchecks(per_seed_forensic);
    let mut tail_names: Vec<String> = FAIL_HOURS_300
        .iter()
        .map(|h| format!("tail_ratio_h{h}"))
        .collect();
    tail_names.push("tail_ratio_all".to_string());
    let a_tail = tail_names
        .iter()
        .any(|name| fires_outside("innovation", metric("innovation", name)));
    let b_init = forensics.initiation_seed_count >= SEED_DIRECTION_MIN;
    let c_controls = forensics.initiation_control_count <= 2;
    rungs.push(RungRec {
        name: "innovation",
        subchecks: vec![
            ("a_tail_ratio", a_tail),
            ("b_initiation", b_init),
            ("c_controls", c_controls),
        ],
        fired: a_tail && b_init && c_controls,
        boundary_localized: None,
        uniform_eligible: None,
        required_resolution: None,
        refusals: Vec::new(),
    });

    // -- Rung 4: signed reversion.
    let sign = closure_analysis(obs, seeds, "sign", mults);
    let a_closure = complete("reversion")
        && sign.all_points_pass
        && sign.joint_lcb.is_some_and(|l| l > GAP_CLOSE_LCB_MIN);
    // Fold stability: each cell's closure sign agrees with that cell's own
    // point closure across every qualifying fold.
    let point_sign: Vec<bool> = sign
        .cells
        .iter()
        .map(|c| c.closure.unwrap_or(0.0) > 0.0)
        .collect();
    let mut folds_ok = !folds.is_empty();
    for f in &folds {
        for (idx, &(hour, h)) in CLOSURE_CELLS.iter().enumerate() {
            let gen_vals: Vec<Option<f64>> = seeds
                .iter()
                .map(|g| weighted_median_votes(&g.b3_votes(hour, h, "robust"), &g.ones()))
                .collect();
            let cl = gap_closure(
                median_or_none(&gen_vals),
                obs.perm_value("sign", hour, h, f),
                obs.b3_robust_strict(hour, h, f),
                false,
            );
            if !cl.is_some_and(|cl| (cl > 0.0) == point_sign[idx]) {
                folds_ok = false;
            }
        }
    }
    let c_cov = HOT_HOURS.iter().all(|&h| {
        let m = metric("reversion", &format!("covnorm_h{h}"));
        !m.refused && m.point.is_some_and(|p| p > 0.0) && m.interval_low.is_some_and(|l| l > 0.0)
    });
    let fired4 = a_closure && folds_ok && c_cov;
    // Amendment E: worsening_23 is evaluated only after the rung fires.
    let mut w23 = None;
    let mut uniform = None;
    let mut resolution = None;
    if fired4 {
        w23 = worsening_23_analysis(obs, seeds, mults);
        match &w23 {
            None => refusals.push(RefusalRec::new(
                "reversion",
                "worsening_23",
                "worsening_23 refused: missing point or incomplete bootstrap population",
            )),
            Some(w) => {
                let u = w.ucb.is_some_and(|u| u <= 0.0);
                uniform = Some(u);
                resolution = Some(if u { "uniform" } else { "hour-resolved" });
            }
        }
    }
    rungs.push(RungRec {
        name: "reversion",
        subchecks: vec![
            ("a_closure", a_closure),
            ("b_folds", folds_ok),
            ("c_covariance", c_cov),
        ],
        fired: fired4,
        boundary_localized: None,
        uniform_eligible: uniform,
        required_resolution: resolution,
        refusals: Vec::new(),
    });

    // -- Rung 5: GARCH persistence.
    let mag = closure_analysis(obs, seeds, "magnitude", mults);
    let a5 = complete("garch")
        && mag.all_points_pass
        && mag.joint_lcb.is_some_and(|l| l > GAP_CLOSE_LCB_MIN);
    let b5 = forensics.escalation_seed_count >= SEED_DIRECTION_MIN
        && forensics
            .control_escalation_median
            .is_some_and(|m| m < CONTROL_ESCALATION_MAX);
    rungs.push(RungRec {
        name: "garch",
        subchecks: vec![("a_closure", a5), ("b_escalation", b5)],
        fired: a5 && b5,
        boundary_localized: None,
        uniform_eligible: None,
        required_resolution: None,
        refusals: Vec::new(),
    });

    // -- Rung 6: boundary-local state. Only reachable when no prior rung
    // fired - a boundary-local explanation is the residual hypothesis.
    let no_prior = !rungs.iter().any(|r| r.fired);
    let mut b_ok = false;
    let mut comp_ok = false;
    for (case, ..) in BOUNDARY_CELLS {
        for stem in ["quote_p99", "robust_60"] {
            let m_b = metric("boundary", &format!("{stem}_{case}"));
            let m_c = metric("boundary", &format!("{stem}_{case}_comparator"));
            if fires_outside("boundary", m_b) && clean_inside("boundary", m_c) {
                b_ok = true;
                comp_ok = true;
            }
        }
    }
    rungs.push(RungRec {
        name: "boundary",
        subchecks: vec![
            ("a_boundary_band", b_ok),
            ("b_comparator_clean", comp_ok),
            ("c_no_prior_rung", no_prior),
        ],
        fired: b_ok && comp_ok && no_prior,
        boundary_localized: None,
        uniform_eligible: None,
        required_resolution: None,
        refusals: Vec::new(),
    });

    // -- Localization (Amendment C). Point estimates only: the flag says
    // where a fired rung's discrepancy lives, and an interval on a ratio of
    // ratios would claim more than the design supports.
    let boundary_log_ratio = |stat_for: &dyn Fn(super::context::Labels) -> StatFn,
                              labels: super::context::Labels|
     -> Option<f64> {
        let stat = stat_for(labels);
        let gen_vals: Vec<Option<f64>> = seeds.iter().map(|g| stat(g, &g.ones())).collect();
        let g = median_or_none(&gen_vals)?;
        let o = stat(obs, &ones)?;
        (g > 0.0 && o > 0.0).then(|| (g / o).ln())
    };
    let localization = |rung_name: &'static str,
                        stat_for: &dyn Fn(super::context::Labels) -> StatFn,
                        refusals: &mut Vec<RefusalRec>|
     -> Option<bool> {
        let mut mags = Vec::new();
        for (case, b_labels, _) in BOUNDARY_CELLS {
            match boundary_log_ratio(stat_for, b_labels) {
                None => {
                    refusals.push(RefusalRec::new(
                        rung_name,
                        format!("localization boundary {case}"),
                        "localization input refused",
                    ));
                    return None;
                }
                Some(v) => mags.push(v.abs()),
            }
        }
        let interior = boundary_log_ratio(stat_for, INTERIOR_LABELS);
        match interior {
            Some(i) if i != 0.0 => {
                Some(mags.into_iter().fold(f64::NEG_INFINITY, f64::max) >= 2.0 * i.abs())
            }
            _ => {
                refusals.push(RefusalRec::new(
                    rung_name,
                    "localization interior",
                    "localization ratio undefined",
                ));
                None
            }
        }
    };
    for rung in &mut rungs {
        let name = rung.name;
        if !rung.fired {
            continue;
        }
        let stat_for: Option<&dyn Fn(super::context::Labels) -> StatFn> = match name {
            "child_walk" => Some(&stat_boundary_excess),
            "reversion" | "garch" => Some(&stat_boundary_robust),
            _ => None,
        };
        if let Some(stat_for) = stat_for {
            rung.boundary_localized = localization(name, stat_for, &mut refusals);
        }
    }
    // The rung mirrors its OWN refusals plus the consumed family's envelope
    // and metric refusals (Amendment D).
    for rung in &mut rungs {
        let mut mirrored: Vec<RefusalRec> = refusals
            .iter()
            .filter(|r| r.scope == rung.name)
            .cloned()
            .collect();
        mirrored.extend(env_of(rung.name).refusals.iter().cloned());
        rung.refusals = mirrored;
    }

    let eligible: Vec<&'static str> = rungs.iter().filter(|r| r.fired).map(|r| r.name).collect();
    let selected = eligible.first().copied();
    let verdict = if eligible.is_empty() {
        "no-family-eligible"
    } else {
        "family-eligible"
    };
    LadderOutcome {
        envelopes,
        count_substitution: csub,
        cond_bins,
        sign_closure: sign,
        magnitude_closure: mag,
        worsening_23: w23,
        forensic_subchecks: forensics,
        rungs,
        eligible,
        selected,
        verdict,
    }
}
