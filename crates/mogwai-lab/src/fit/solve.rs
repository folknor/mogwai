// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The CRN solve machinery of the retired Python fit implementation (protocol-10 spec 4.75,
//! carried into protocol 11): `coarse_grid`, `trisect` and `solve_scalar`.
//!
//! The invariants this module exists to preserve, all of them load-bearing
//! for reproducing a solve from a walk cache:
//!
//! 1. **Seeded endpoints are never re-evaluated.** The coarse grid already
//!    paid for the bracket endpoints and the coarse winner; under common
//!    random numbers a re-evaluation is bit-identical and therefore pure
//!    waste - here, whole multi-minute generator walks.
//! 2. **The fresh interior pair alone decides the bracket.** An incumbent
//!    (endpoint or seed) carries no directional information; letting one
//!    vote provably drags the bracket off the optimum.
//! 3. **Flat objectives tie-break to the smaller candidate**, compared in
//!    the transformed domain (log x when `log_domain`).
//! 4. **Log-domain termination is relative by construction**: `a` and `b`
//!    are logs, so their span is `log(hi/lo)`, compared against
//!    `log1p(SOLVE_RELATIVE_STEP)`. Dividing a log span by `|log x|` is not
//!    a relative error in `x`; it over-refines near 1 and under-refines far
//!    from it.
//! 5. **Deterministic end to end by construction** - no randomness anywhere
//!    in the search, so two runs name the same candidates in the same order.
//!
//! Python's `search_score` may be a tuple in the general shape; every live
//! protocol-11 objective is a scalar, so this port carries `f64` alone and
//! the `list(...)`-if-tuple branch has no counterpart.

use serde_json::{Value, json};

use crate::subcontract::SOLVE_RELATIVE_STEP;

/// Boundary slack for the inclusive comparisons, the pair-harness
/// convention: a bound like 0.10 is not exactly representable in binary, so
/// a discrepancy of exactly-the-bound computes a hair above it.
pub const SLACK: f64 = 1e-12;

/// The deterministic coarse grid, shared by `solve_scalar` and the prewarm
/// calls so both name exactly the same candidates.
#[must_use]
pub fn coarse_grid(lo: f64, hi: f64, points: usize, log_domain: bool) -> Vec<f64> {
    if log_domain {
        let step = (hi.ln() - lo.ln()) / ((points - 1) as f64);
        (0..points)
            .map(|i| (lo.ln() + (i as f64) * step).exp())
            .collect()
    } else {
        let step = (hi - lo) / ((points - 1) as f64);
        (0..points).map(|i| lo + (i as f64) * step).collect()
    }
}

/// The outcome of a `trisect` refinement.
pub struct Trisection {
    pub best_candidate: f64,
    pub best_score: f64,
    pub termination: String,
    pub evaluations: usize,
}

/// 4.75 refinement: classic ternary comparison with coarse-score seeding.
///
/// Each iteration evaluates the two trisection points and keeps `[a, m2]`
/// when `f(m1) <= f(m2)` (the tie keeps the left), else `[m1, b]`. The
/// returned candidate is the best point ever evaluated, smaller winning
/// score ties.
pub fn trisect(
    evaluate: &mut dyn FnMut(f64) -> f64,
    lo: f64,
    hi: f64,
    log_domain: bool,
    absolute_step: Option<f64>,
    objective_threshold: Option<f64>,
    seeds: &[(f64, f64)],
) -> Trisection {
    let xform = |x: f64| if log_domain { x.ln() } else { x };
    let unxform = |x: f64| if log_domain { x.exp() } else { x };

    let mut a = xform(lo);
    let mut b = xform(hi);
    let mut best_x: Option<f64> = None;
    let mut best_score: Option<f64> = None;
    let mut evaluations = 0usize;

    // `record` mirrors Python's closure exactly, including the tie-break on
    // the transformed coordinate.
    macro_rules! record {
        ($x:expr, $score:expr) => {{
            let x: f64 = $x;
            let score: f64 = $score;
            let better = match (best_score, best_x) {
                (None, _) => true,
                (Some(bs), Some(bx)) => score < bs || (score == bs && x < bx),
                (Some(_), None) => true,
            };
            if better {
                best_x = Some(x);
                best_score = Some(score);
            }
            score
        }};
    }

    // Python builds `seeded` as a dict keyed on the transformed coordinate:
    // duplicate keys collapse with the last value winning, and iteration is
    // insertion-ordered. A Vec with last-wins replacement reproduces both.
    let mut seeded: Vec<(f64, f64)> = Vec::new();
    for (x, s) in seeds {
        let k = xform(*x);
        match seeded.iter_mut().find(|(kk, _)| *kk == k) {
            Some(slot) => slot.1 = *s,
            None => seeded.push((k, *s)),
        }
    }
    let seeded_score = |k: f64| seeded.iter().find(|(kk, _)| *kk == k).map(|(_, s)| *s);

    for endpoint in [a, b] {
        match seeded_score(endpoint) {
            // A known score is recorded, never re-evaluated (invariant 1).
            Some(s) => {
                record!(endpoint, s);
            }
            None => {
                evaluations += 1;
                let s = evaluate(unxform(endpoint));
                record!(endpoint, s);
            }
        }
    }
    // The coarse winner between the endpoints: recorded so best-ever
    // tracking stays honest when refinement never beats it.
    for (x, s) in &seeded {
        if *x != a && *x != b {
            record!(*x, *s);
        }
    }

    let termination = loop {
        if let Some(t) = objective_threshold
            && best_score.expect("at least one endpoint recorded") <= t + SLACK
        {
            break format!("objective <= {}", crate::kernel::py_float_repr(t));
        }
        let span = b - a;
        if let Some(step) = absolute_step {
            if span <= step {
                break format!("absolute step <= {}", crate::kernel::py_float_repr(step));
            }
        } else if log_domain {
            // Invariant 4: the log span IS the relative width.
            if span <= SOLVE_RELATIVE_STEP.ln_1p() {
                break format!(
                    "relative step <= {}",
                    crate::kernel::py_float_repr(SOLVE_RELATIVE_STEP)
                );
            }
        } else {
            let mid_abs = a.abs().max(b.abs()).max(1e-30);
            if span / mid_abs <= SOLVE_RELATIVE_STEP {
                break format!(
                    "relative step <= {}",
                    crate::kernel::py_float_repr(SOLVE_RELATIVE_STEP)
                );
            }
        }
        let m1 = a + span / 3.0;
        let m2 = a + 2.0 * span / 3.0;
        evaluations += 1;
        let f1 = record!(m1, evaluate(unxform(m1)));
        evaluations += 1;
        let f2 = record!(m2, evaluate(unxform(m2)));
        // Invariant 2: only this fresh pair moves the bracket; the tie keeps
        // the left.
        if f1 <= f2 {
            b = m2;
        } else {
            a = m1;
        }
    };

    Trisection {
        best_candidate: unxform(best_x.expect("recorded")),
        best_score: best_score.expect("recorded"),
        termination,
        evaluations,
    }
}

/// Coarse grid then trisection of the winner's neighbor bracket; a boundary
/// winner takes its single inside neighbor interval. Returns the solve
/// record the artifact schema requires.
pub fn solve_scalar(
    evaluate: &mut dyn FnMut(f64) -> f64,
    lo: f64,
    hi: f64,
    points: usize,
    log_domain: bool,
    absolute_step: Option<f64>,
    objective_threshold: Option<f64>,
) -> Value {
    let grid = coarse_grid(lo, hi, points, log_domain);
    let scores: Vec<f64> = grid.iter().map(|x| evaluate(*x)).collect();
    // Python's `min(range(n), key=lambda i: (scores[i], grid[i]))`: smaller
    // score wins, then smaller candidate.
    let mut best_i = 0usize;
    for i in 1..grid.len() {
        let cur = (scores[best_i], grid[best_i]);
        let cand = (scores[i], grid[i]);
        if cand.0 < cur.0 || (cand.0 == cur.0 && cand.1 < cur.1) {
            best_i = i;
        }
    }
    let left = grid[best_i.saturating_sub(1)];
    let right = grid[(best_i + 1).min(grid.len() - 1)];
    let tie_break = "smaller candidate on equal scores";

    if let Some(t) = objective_threshold
        && scores[best_i] <= t + SLACK
    {
        return json!({
            "domain": [lo, hi],
            "coarse_points": points,
            "coarse_grid": grid,
            "best_candidate": grid[best_i],
            "search_score": scores[best_i],
            "termination": format!("objective <= {}", crate::kernel::py_float_repr(t)),
            "tie_break": tie_break,
            "evaluations": points,
        });
    }
    if left == right {
        return json!({
            "domain": [lo, hi],
            "coarse_points": points,
            "coarse_grid": grid,
            "best_candidate": grid[best_i],
            "search_score": scores[best_i],
            "termination": "degenerate single-point domain",
            "tie_break": tie_break,
            "evaluations": points,
        });
    }
    let out = trisect(
        evaluate,
        left,
        right,
        log_domain,
        absolute_step,
        objective_threshold,
        &[
            (left, scores[best_i.saturating_sub(1)]),
            (grid[best_i], scores[best_i]),
            (right, scores[(best_i + 1).min(grid.len() - 1)]),
        ],
    );
    json!({
        "domain": [lo, hi],
        "coarse_points": points,
        "coarse_grid": grid,
        "best_candidate": out.best_candidate,
        "search_score": out.best_score,
        "termination": out.termination,
        "tie_break": tie_break,
        "evaluations": points + out.evaluations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best(v: &Value) -> f64 {
        v["best_candidate"].as_f64().unwrap()
    }

    /// the retired Python fit implementation selftest: "trisection converges on a plain objective".
    #[test]
    fn trisection_converges_on_a_plain_objective() {
        let solve = solve_scalar(&mut |x| (x - 3.2).abs(), 0.0, 10.0, 11, false, None, None);
        assert!((best(&solve) - 3.2).abs() < 0.01);
    }

    /// "a flat objective tie-breaks to the smaller candidate".
    #[test]
    fn a_flat_objective_tie_breaks_to_the_smaller_candidate() {
        let solve = solve_scalar(&mut |_| 0.0, 0.0, 10.0, 11, false, None, None);
        assert!(best(&solve) <= 1.0);
    }

    /// "a boundary winner refines its single inside neighbor interval".
    #[test]
    fn a_boundary_winner_refines_its_single_inside_neighbor_interval() {
        let solve = solve_scalar(&mut |x| -x, 0.0, 10.0, 11, false, None, None);
        assert!((best(&solve) - 10.0).abs() < 0.02);
    }

    /// "seeded endpoints are never re-evaluated" and "the fresh interior
    /// pair, not the seeds, decides the bracket".
    #[test]
    fn seeded_endpoints_are_never_re_evaluated_and_the_fresh_pair_decides() {
        let mut calls: Vec<f64> = Vec::new();
        let out = {
            let mut f = |x: f64| {
                calls.push(x);
                (x - 2.0).abs()
            };
            trisect(
                &mut f,
                0.0,
                3.0,
                false,
                Some(0.1),
                None,
                &[(0.0, 0.5), (3.0, 1.0)],
            )
        };
        assert!(!calls.contains(&0.0), "seeded lo endpoint re-evaluated");
        assert!(!calls.contains(&3.0), "seeded hi endpoint re-evaluated");
        assert!(
            (out.best_candidate - 2.0).abs() < 0.1,
            "the seeds, not the fresh interior pair, decided the bracket"
        );
    }

    /// "an objective threshold stops after the coarse grid".
    #[test]
    fn an_objective_threshold_stops_after_the_coarse_grid() {
        let solve = solve_scalar(
            &mut |x| (x - 3.2).abs(),
            0.0,
            10.0,
            11,
            false,
            None,
            Some(0.25),
        );
        assert_eq!(solve["termination"], "objective <= 0.25");
        assert_eq!(solve["evaluations"], 11);
    }

    /// "log-domain relative termination reads the log span directly".
    #[test]
    fn log_domain_relative_termination_reads_the_log_span_directly() {
        let solve = solve_scalar(&mut |_| 0.0, 1e-8, 1e-4, 11, true, None, None);
        assert_eq!(
            solve["termination"],
            format!(
                "relative step <= {}",
                crate::kernel::py_float_repr(SOLVE_RELATIVE_STEP)
            )
        );
        assert!(best(&solve) <= 1.001e-8);
        assert!(solve["evaluations"].as_u64().unwrap() < 60);
    }

    /// "the search is deterministic end to end (CRN by construction)".
    #[test]
    fn the_search_is_deterministic_end_to_end() {
        let mut first: Vec<f64> = Vec::new();
        solve_scalar(
            &mut |x| {
                first.push(x);
                (x - 5.0).abs()
            },
            0.0,
            10.0,
            11,
            false,
            None,
            None,
        );
        let mut second: Vec<f64> = Vec::new();
        solve_scalar(
            &mut |x| {
                second.push(x);
                (x - 5.0).abs()
            },
            0.0,
            10.0,
            11,
            false,
            None,
            None,
        );
        assert_eq!(first, second);
    }

    /// The coarse grid is the shared candidate namer: prewarm and solve must
    /// produce bit-identical points, or the cache misses on every walk.
    #[test]
    fn the_coarse_grid_endpoints_are_exact_in_both_domains() {
        let g = coarse_grid(0.0, 10.0, 11, false);
        assert_eq!(g.len(), 11);
        assert_eq!(g[0], 0.0);
        assert_eq!(g[10], 10.0);
        let lg = coarse_grid(1e-8, 1e-4, 32, true);
        assert_eq!(lg.len(), 32);
        // `exp(log(lo))` is not exactly `lo`; the Python's grid carries the
        // same round trip, and the cache keys hash the round-tripped value,
        // so the endpoints are compared relatively here rather than pinned
        // to the literal.
        assert!((lg[0] / 1e-8 - 1.0).abs() < 1e-12);
        assert!((lg[31] / 1e-4 - 1.0).abs() < 1e-12);
    }
}
