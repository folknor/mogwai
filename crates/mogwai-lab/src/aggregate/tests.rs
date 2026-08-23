// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Unit tests for the phase-2b semantics that the artifact parity gate
//! cannot reach on its own. The gate proves the whole pipeline reproduces
//! one committed artifact; these pin the individual rules at the corners
//! that artifact happens not to exercise, so a future edit that breaks a
//! rule fails here by name rather than as one diverging float.

use serde_json::json;

use super::bootstrap::{
    QuantileSupport, bootstrap_multiplicities, fold_multiplicities, iso_year_week,
    stage_m_bootstrap_multiplicities, weighted_median_votes,
};
use super::context::ObsContext;
use super::countsub::gap_closure;
use super::family::{Kind, MetricDef, Predicate, evaluate_family, log_band};
use super::monthly::{central_blocks_from_seeds, tree_median};
use crate::subcontract::BOOTSTRAP_REPLICATES;

// -- The bootstrap ----------------------------------------------------------

#[test]
fn bootstrap_multiplicities_are_the_frozen_draw() {
    let mults = bootstrap_multiplicities(22);
    assert_eq!(mults.len(), BOOTSTRAP_REPLICATES as usize);
    for m in &mults {
        assert_eq!(m.len(), 22);
        // Five blocks of five, truncated to the session count.
        assert_eq!(m.iter().sum::<i64>(), 22);
    }
    // Bit-reproducibility: the same call twice, and the first replicate's
    // block starts transcribed from the frozen rule.
    assert_eq!(mults, bootstrap_multiplicities(22));
    let mut expect = vec![0i64; 22];
    let mut drawn = 0;
    let base = crate::subcontract::BOOTSTRAP_BASE_SEED as u64;
    for block in 0..5u64 {
        let start = (crate::kernel::splitmix64(base ^ block) % 22) as usize;
        for k in 0..5 {
            if drawn >= 22 {
                break;
            }
            expect[(start + k) % 22] += 1;
            drawn += 1;
        }
    }
    assert_eq!(mults[0], expect);
}

#[test]
fn stage_m_bootstrap_fills_each_month_and_is_month_separated() {
    let a = stage_m_bootstrap_multiplicities(202_508, 27);
    let b = stage_m_bootstrap_multiplicities(202_509, 27);
    assert_eq!(a.len(), BOOTSTRAP_REPLICATES as usize);
    assert!(
        a.iter()
            .all(|m| m.len() == 27 && m.iter().sum::<i64>() == 27)
    );
    assert_ne!(a[0], b[0]);
    assert_eq!(a, stage_m_bootstrap_multiplicities(202_508, 27));
}

#[test]
#[should_panic(expected = "July uses the original bootstrap domain")]
fn stage_m_bootstrap_refuses_july_domain_collision() {
    drop(stage_m_bootstrap_multiplicities(202_607, 22));
}

#[test]
fn iso_weeks_match_the_python_isocalendar() {
    // Transcribed from datetime.date.fromisoformat(x).isocalendar().
    assert_eq!(iso_year_week("2026-07-01"), (2026, 27));
    assert_eq!(iso_year_week("2026-07-05"), (2026, 27));
    assert_eq!(iso_year_week("2026-07-06"), (2026, 28));
    // The year boundary, where the ISO week-year differs from the calendar
    // year in both directions.
    assert_eq!(iso_year_week("2027-01-01"), (2026, 53));
    assert_eq!(iso_year_week("2024-12-30"), (2025, 1));
}

#[test]
fn folds_drop_one_week_and_refuse_below_the_floor() {
    // Four ISO weeks of five sessions: dropping any one leaves 15, exactly
    // the floor, so all four qualify.
    let sessions: Vec<String> = (0..4)
        .flat_map(|w| (0..5).map(move |d| format!("2026-07-{:02}", 6 + w * 7 + d)))
        .collect();
    let folds = fold_multiplicities(&sessions);
    assert_eq!(folds.len(), 4);
    for f in &folds {
        assert_eq!(f.iter().sum::<i64>(), 15);
    }
    // Three weeks of five: dropping one leaves 10, below the floor, so NO
    // fold qualifies and every fold-dependent rule fails closed.
    let short: Vec<String> = sessions[..15].to_vec();
    assert!(fold_multiplicities(&short).is_empty());
}

#[test]
fn weighted_median_votes_is_the_ceil_half_order_statistic() {
    let votes = [Some(1.0), Some(2.0), Some(3.0)];
    assert_eq!(weighted_median_votes(&votes, &[1, 1, 1]), Some(2.0));
    // A zero multiplicity removes the session entirely; an even total takes
    // the ceil(total/2)-th, i.e. the upper of the two middles here.
    assert_eq!(weighted_median_votes(&votes, &[1, 0, 1]), Some(1.0));
    assert_eq!(weighted_median_votes(&votes, &[0, 1, 3]), Some(3.0));
    // A None vote contributes nothing rather than refusing.
    assert_eq!(
        weighted_median_votes(&[None, Some(5.0)], &[1, 1]),
        Some(5.0)
    );
    assert_eq!(weighted_median_votes(&[None], &[1]), None);
    assert_eq!(weighted_median_votes(&votes, &[0, 0, 0]), None);
}

#[test]
fn quantile_support_pools_across_sessions_under_multiplicities() {
    let per = vec![vec![(1.0, 90i64), (2.0, 10)], vec![(3.0, 100i64)]];
    let qs = QuantileSupport::new(&per);
    // Session 2 alone: every quantile is 3.
    assert_eq!(qs.quantile(0.5, &[0, 1]), Some(3.0));
    // Session 1 alone: p90 lands on 1, p99 on 2.
    assert_eq!(qs.quantile(0.90, &[1, 0]), Some(1.0));
    assert_eq!(qs.quantile(0.99, &[1, 0]), Some(2.0));
    // Both, with session 2 counted twice: 90 + 10 + 200 weight.
    assert_eq!(qs.quantile(0.5, &[1, 2]), Some(3.0));
    assert_eq!(qs.quantile(0.0, &[1, 1]), Some(1.0));
    assert_eq!(qs.quantile(0.5, &[0, 0]), None);
    assert_eq!(QuantileSupport::new(&[vec![]]).quantile(0.5, &[1]), None);
}

// -- gap_closure ------------------------------------------------------------

#[test]
fn gap_closure_refuses_nonpositive_inputs_and_a_vanishing_gap() {
    // Generated 4, observed 1, counterfactual 2: half the log gap closed
    // from either side.
    let c = gap_closure(Some(4.0), Some(2.0), Some(1.0), true).expect("a closure");
    assert!((c - 0.5).abs() < 1e-12);
    let c = gap_closure(Some(4.0), Some(2.0), Some(1.0), false).expect("a closure");
    assert!((c - 0.5).abs() < 1e-12);
    assert_eq!(gap_closure(None, Some(2.0), Some(1.0), true), None);
    assert_eq!(gap_closure(Some(0.0), Some(2.0), Some(1.0), true), None);
    assert_eq!(gap_closure(Some(-1.0), Some(2.0), Some(1.0), true), None);
    // A denominator under GAP_CLOSE_EPS refuses rather than exploding.
    assert_eq!(gap_closure(Some(1.0), Some(2.0), Some(1.0), true), None);
}

// -- Amendment D ------------------------------------------------------------

/// A context whose sessions each carry one block-3 hour-19 300 s cell.
fn ctx_with_robusts(cells: &[(f64, i64)]) -> ObsContext {
    ObsContext::new(
        cells
            .iter()
            .enumerate()
            .map(|(i, &(value, count))| session_json(i, value, count))
            .collect(),
    )
}

/// A one-session context, the degenerate case.
fn ctx_with_robust(value: f64, count: i64) -> ObsContext {
    ctx_with_robusts(&[(value, count)])
}

fn session_json(i: usize, value: f64, count: i64) -> serde_json::Value {
    json!({
        "session_date": format!("2026-07-{:02}", i + 1),
        "block1_hist": [],
        "block2": {},
        "block3": {
            "cells": {"19": {"300": {
                "return_count": count,
                "robust_scale": value,
                "rms_scale": value,
            }}},
            "pairs": {},
            "lag1_parent_autocorr": {},
            "hour20_labels": {},
        },
        "block4": {},
        "permutations": [],
        "refusals": [],
    })
}

fn metric_def(name: &str, forced: Option<&str>, seed_value: f64) -> MetricDef {
    MetricDef {
        name: name.to_string(),
        kind: Kind::LogRatio,
        predicate: Predicate::Outside,
        stat: super::family::stat_robust(19, 300),
        gen_seeds: vec![Some(seed_value); 8],
        gen_central: Some(seed_value),
        qualify_refusals: Vec::new(),
        force_refused: forced.map(str::to_string),
    }
}

#[test]
fn amendment_d_keeps_computable_evidence_and_nulls_only_the_envelope() {
    // Two sessions whose votes differ, so the two replicates below produce
    // two distinct log ratios and the SE is nonzero.
    let obs = ctx_with_robusts(&[(1.0, 100), (2.0, 100)]);
    let mults = vec![vec![1i64, 1], vec![0i64, 2]];
    let folds = vec![vec![1i64, 1]];
    let ones = vec![1i64, 1];
    let defs = vec![
        metric_def("computable", None, 4.0),
        metric_def("forced", Some("required observed bin without support"), 4.0),
    ];
    let env = evaluate_family("arrival", &defs, &obs, &mults, &folds, &ones);
    assert!(!env.inventory_complete);
    assert_eq!(env.critical_value, None);

    let forced = env.metric("forced");
    assert!(forced.refused);
    assert_eq!(forced.point, None);
    assert_eq!(forced.se, None);
    assert_eq!(forced.seed_rule_pass, None);

    let good = env.metric("computable");
    assert!(!good.refused);
    // The point, the band, the point-only predicate and the seed and fold
    // evidence all survive...
    assert!(good.point.is_some_and(|p| (p - 4.0f64.ln()).abs() < 1e-12));
    assert_eq!(good.band_low, Some(log_band().0));
    assert_eq!(good.outside_band, Some(true));
    assert_eq!(good.seed_same_side_count, Some(8));
    assert_eq!(good.seed_rule_pass, Some(true));
    assert_eq!(good.fold_rule_pass, Some(true));
    // ...and only the envelope fields go null.
    assert_eq!(good.interval_low, None);
    assert_eq!(good.interval_high, None);
    assert_eq!(good.envelope_excludes_edge, None);

    // Exactly two refusals: the forced metric and the one envelope record
    // that owns the envelope-only nulls.
    assert_eq!(env.refusals.len(), 2);
    assert_eq!(env.refusals[0].cell, "forced");
    assert_eq!(env.refusals[0].scope, "family:arrival");
    assert_eq!(env.refusals[1].cell, "envelope");
}

#[test]
fn every_cause_of_one_refused_metric_aggregates_into_one_record() {
    let obs = ctx_with_robust(1.0, 100);
    let mut def = metric_def("many_causes", Some("forced reason"), 4.0);
    def.qualify_refusals = vec!["first line".to_string(), "second line".to_string()];
    let env = evaluate_family("garch", &[def], &obs, &[vec![1]], &[vec![1]], &[1]);
    // One metric refusal plus the envelope record - never one per cause.
    assert_eq!(env.refusals.len(), 2);
    assert_eq!(
        env.refusals[0].reason,
        "forced reason; first line; second line"
    );
}

#[test]
fn a_below_floor_session_vote_refuses_the_metric_not_just_the_replicate() {
    // The one session's cell sits below MIN_300S_CELL_RETURNS, so the vote
    // is None, the point input is missing and the metric refuses.
    let obs = ctx_with_robust(1.0, 1);
    let env = evaluate_family(
        "reversion",
        &[metric_def("below_floor", None, 4.0)],
        &obs,
        &[vec![1]],
        &[vec![1]],
        &[1],
    );
    assert!(env.metric("below_floor").refused);
    assert!(!env.inventory_complete);
    assert_eq!(
        env.refusals[0].reason,
        "nonpositive or missing point inputs"
    );
}

// -- The strict accessors ---------------------------------------------------

#[test]
fn b3_robust_strict_refuses_on_any_missing_vote_whatever_the_multiplicity() {
    let mut recs = ctx_with_robust(1.0, 100).per_session().to_vec();
    let mut short = recs[0].clone();
    short["session_date"] = json!("2026-07-02");
    short["block3"]["cells"]["19"]["300"]["return_count"] = json!(1);
    recs.push(short);
    let ctx = ObsContext::new(recs);
    // The lenient path happily medians the one qualifying session...
    assert_eq!(
        weighted_median_votes(&ctx.b3_votes(19, 300, "robust"), &[1, 1]),
        Some(1.0)
    );
    // ...and the strict path refuses, even under a multiplicity vector that
    // weights the offending session to zero (the no-K-of-N ruling).
    assert_eq!(ctx.b3_robust_strict(19, 300, &[1, 1]), None);
    assert_eq!(ctx.b3_robust_strict(19, 300, &[1, 0]), None);
}

// -- tree_median and the union-zero centralization --------------------------

#[test]
fn tree_median_preserves_leaf_types_and_refuses_shape_drift() {
    let a = json!({"k": 1, "s": "x"});
    let b = json!({"k": 3, "s": "x"});
    let c = json!({"k": 2, "s": "x"});
    let med = tree_median(&[&a, &b, &c]).expect("a median");
    // An integer population medians to an integer leaf, not a float.
    assert_eq!(crate::kernel::typed_canon(&med["k"]), "[\"i\", 2]");
    // An even population takes the lower of the two middles.
    let med = tree_median(&[&a, &b]).expect("a median");
    assert_eq!(crate::kernel::typed_canon(&med["k"]), "[\"i\", 1]");
    // Any null centralizes the leaf to null - no median over fewer seeds.
    let n = json!({"k": null, "s": "x"});
    assert!(tree_median(&[&a, &n]).expect("a median")["k"].is_null());
    // A key-set mismatch refuses, and so does a disagreeing string.
    let d = json!({"k": 1});
    assert!(tree_median(&[&a, &d]).is_err());
    let e = json!({"k": 1, "s": "y"});
    assert!(tree_median(&[&a, &e]).is_err());
}

#[test]
fn central_blocks_pad_only_the_two_histogram_supports() {
    let seed = |hist: serde_json::Value, run: serde_json::Value, sched: i64| {
        json!({
            "block1": {"hist": [1, 2, 3], "summary": {}, "by_labels": {}},
            "block2": {"19": {"60": {
                "scheduled_windows": sched,
                "count_hist": hist,
                "run_length_hist": run,
            }}},
            "block3": {},
            "block4": {},
        })
    };
    let a = seed(json!({"0": 4, "1": 2}), json!({"1": 1}), 59);
    let b = seed(json!({"1": 6, "2": 8}), json!({"2": 3}), 59);
    let central = central_blocks_from_seeds(&[&a, &b]).expect("a central block");
    // block1.hist is stripped, never centralized.
    assert!(central["block1"].get("hist").is_none());
    let cell = &central["block2"]["19"]["60"];
    // The support is the union; an absent value reads as a zero count, and
    // the two-seed median takes the lower middle.
    assert_eq!(cell["count_hist"], json!({"0": 0, "1": 2, "2": 0}));
    assert_eq!(cell["run_length_hist"], json!({"1": 0, "2": 0}));
    assert_eq!(cell["scheduled_windows"], json!(59));
    // The per-seed evidence is untouched: the padding worked on copies.
    assert_eq!(
        a["block2"]["19"]["60"]["count_hist"],
        json!({"0": 4, "1": 2})
    );

    // A cell missing one of the two histogram fields refuses before any
    // padding - only the support of a present histogram may vary.
    let broken = json!({
        "block1": {"hist": [], "summary": {}, "by_labels": {}},
        "block2": {"19": {"60": {"scheduled_windows": 59, "count_hist": {"0": 1}}}},
        "block3": {}, "block4": {},
    });
    assert!(central_blocks_from_seeds(&[&a, &broken]).is_err());
}

// -- The calendar exposure judge --------------------------------------------

#[test]
fn expected_scheduled_windows_is_calendar_derived() {
    use crate::session::expected_scheduled_windows;
    // A full UTC hour inside the overnight segment carries 59 one-minute
    // windows: the sixtieth would end ON the hour boundary and therefore
    // crosses under endpoint-hour attribution.
    assert_eq!(expected_scheduled_windows("2026-07-01", 4, 60), 59);
    // The halt hour is truncated by the 15:15 local segment end.
    assert_eq!(expected_scheduled_windows("2026-07-01", 20, 60), 44);
    // An hour the session never spans expects nothing at all.
    assert_eq!(expected_scheduled_windows("2026-07-01", 21, 60), 0);
}
