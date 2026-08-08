// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12b Stage A: the necessary-condition screen.
//!
//! Corpus-free by construction. Both observed projections come from the
//! committed protocol-12a artifact (`analysis/mnq-measure-12a.json`), which
//! already carries the exact sparse joint histogram keyed on exact parent
//! count and the per-hour count histograms at 1 s, 5 s and 60 s. Nothing here
//! reads a TBBO corpus, and nothing here consumes the brick-B4 envelope: the
//! binding admissibility list is A1 to A4 and B4 is a Stage B gate.
//!
//! The spec is `notes/protocol-12b-arrival-composition-spec.md`; sections 9
//! and 16 own everything in this module.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{LabError, LabResult};

/// The six frozen protocol-12a parent-count bins, as half-open intervals over
/// the exact count. `MIN_MINUTES_CELL` and the bin edges are 12a's, not this
/// protocol's, and are never re-derived here.
pub const PARENT_COUNT_BINS: [(u32, u32); 6] = [
    (0, 0),
    (1, 64),
    (65, 256),
    (257, 1024),
    (1025, 4096),
    (4097, u32::MAX),
];

/// 12a section 3.3. A required bin is one whose pooled OBSERVED populated
/// minute count reaches this floor.
pub const MIN_MINUTES_CELL: u64 = 30;

/// 12a section 2.1's frozen hour set, the only hours where A1's conditional
/// limb applies.
pub const FAIL_HOURS_300: [u32; 3] = [19, 20, 23];

/// The name of a bin, in the artifact's own spelling.
#[must_use]
pub fn bin_name(n: u32) -> &'static str {
    match n {
        0 => "0",
        1..=64 => "1-64",
        65..=256 => "65-256",
        257..=1024 => "257-1024",
        1025..=4096 => "1025-4096",
        _ => "4097+",
    }
}

/// The per-hour distribution of the EXACT parent count over populated
/// minutes, as `(count, occurrences)` pairs sorted ascending by count.
///
/// Exact `n` is retained rather than binned: the six bins coarsen only the
/// support check and the reported diagnostics, never the loss.
pub type CountMarginal = BTreeMap<u32, Vec<(u32, u64)>>;

/// Marginalizes a protocol-12a `block1.hist` down to the parent-count axis.
///
/// The histogram rows carry the two range axes and both segment-label axes as
/// well; every one of them is summed out here, because Stage A may not claim
/// to evaluate anything that needs the price or book path.
///
/// # Errors
/// Refuses a row missing `hour`, `n` or `count`, rather than skipping it: a
/// silently dropped row would understate support, which is the one direction
/// that turns an inadmissible cell admissible.
pub fn parent_count_marginal(hist: &Value) -> LabResult<CountMarginal> {
    let rows = hist
        .as_array()
        .ok_or_else(|| LabError::refusal("block1.hist is not an array"))?;
    let mut acc: BTreeMap<u32, BTreeMap<u32, u64>> = BTreeMap::new();
    for row in rows {
        let hour = u32::try_from(
            row["hour"]
                .as_u64()
                .ok_or_else(|| LabError::refusal("block1.hist row without an integer hour"))?,
        )
        .map_err(|_| LabError::refusal("block1.hist hour out of range"))?;
        let n = u32::try_from(
            row["n"]
                .as_u64()
                .ok_or_else(|| LabError::refusal("block1.hist row without an integer n"))?,
        )
        .map_err(|_| LabError::refusal("block1.hist n out of range"))?;
        let count = row["count"]
            .as_u64()
            .ok_or_else(|| LabError::refusal("block1.hist row without an integer count"))?;
        *acc.entry(hour).or_default().entry(n).or_default() += count;
    }
    Ok(acc
        .into_iter()
        .map(|(hour, counts)| (hour, counts.into_iter().collect()))
        .collect())
}

/// Pooled populated-minute count per bin for one hour.
#[must_use]
pub fn bin_totals(marginal: &[(u32, u64)]) -> BTreeMap<&'static str, u64> {
    let mut totals: BTreeMap<&'static str, u64> = BTreeMap::new();
    for &(n, count) in marginal {
        *totals.entry(bin_name(n)).or_default() += count;
    }
    totals
}

/// The exact 1-Wasserstein distance between two empirical distributions of
/// `log1p(n)`, each given as `(count, occurrences)` pairs.
///
/// Computed from the sorted empirical CDFs with no binning, as
/// `integral |F - G| dx` over the shared support: walk the union of support
/// points in ascending order and accumulate the absolute CDF gap across each
/// interval between consecutive points.
///
/// `log1p` is frozen by section 9.3 because parent counts span three orders of
/// magnitude and an untransformed distance would be dominated by the busiest
/// hour. This is a RANKING device: it never stands as evidence that the raw
/// count distributions agree, which is A1's job.
///
/// Returns zero for two empty populations and `None` if exactly one side is
/// empty, since a distance to nothing is undefined rather than large.
#[must_use]
pub fn wasserstein_log1p(left: &[(u32, u64)], right: &[(u32, u64)]) -> Option<f64> {
    let left_total: u64 = left.iter().map(|&(_, c)| c).sum();
    let right_total: u64 = right.iter().map(|&(_, c)| c).sum();
    match (left_total, right_total) {
        (0, 0) => return Some(0.0),
        (0, _) | (_, 0) => return None,
        _ => {}
    }

    let mut support: Vec<u32> = left.iter().map(|&(n, _)| n).collect();
    support.extend(right.iter().map(|&(n, _)| n));
    support.sort_unstable();
    support.dedup();

    let cdf = |side: &[(u32, u64)], total: u64, upto: u32| -> f64 {
        let seen: u64 = side
            .iter()
            .filter(|&&(n, _)| n <= upto)
            .map(|&(_, c)| c)
            .sum();
        seen as f64 / total as f64
    };

    let mut distance = 0.0;
    for window in support.windows(2) {
        let (lo, hi) = (window[0], window[1]);
        let gap = f64::from(hi).ln_1p() - f64::from(lo).ln_1p();
        let left_cdf = cdf(left, left_total, lo);
        let right_cdf = cdf(right, right_total, lo);
        distance += (left_cdf - right_cdf).abs() * gap;
    }
    Some(distance)
}

/// Section 16's `linear(lo, hi, step)`.
///
/// Values are computed as `lo + i * step` from an integer index rather than by
/// accumulating `step`, so a long grid cannot drift off its stated literals.
#[must_use]
pub fn linear_grid(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let mut points = Vec::new();
    let mut index = 0_u32;
    loop {
        let value = f64::from(index).mul_add(step, lo);
        if value > hi * (1.0 + 1e-12) {
            break;
        }
        points.push(value);
        index += 1;
    }
    points
}

/// Section 16's `logk(lo, hi, k)`: the points `lo * 10^(j/k)` while they stay
/// at or below `hi`, with `hi` appended when the last generated point falls
/// short of it by more than the stated tolerance.
///
/// Each value is computed from the literal `lo`, `j` and `k` rather than
/// chained, for the same reason `linear_grid` indexes rather than accumulates.
#[must_use]
pub fn log_grid(lo: f64, hi: f64, k: u32) -> Vec<f64> {
    let mut points = Vec::new();
    let mut j = 0_u32;
    loop {
        let value = lo * 10_f64.powf(f64::from(j) / f64::from(k));
        if value > hi * (1.0 + 1e-12) {
            break;
        }
        points.push(value);
        j += 1;
    }
    if points.last().is_none_or(|&last| last < hi * (1.0 - 1e-12)) {
        points.push(hi);
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_marginal_sums_every_axis_except_the_parent_count() {
        // Two rows sharing an hour and a count but differing on the range and
        // label axes must pool: Stage A sees the count axis and nothing else.
        let hist = json!([
            {"hour": 19, "n": 5, "count": 2, "trade_range_ticks": 3,
             "quote_range_half_ticks": 1, "since_open_bin": "0-300",
             "until_close_bin": "1800+"},
            {"hour": 19, "n": 5, "count": 3, "trade_range_ticks": 9,
             "quote_range_half_ticks": 4, "since_open_bin": "300-1800",
             "until_close_bin": "0-300"},
            {"hour": 20, "n": 7, "count": 1, "trade_range_ticks": 2,
             "quote_range_half_ticks": 0, "since_open_bin": "1800+",
             "until_close_bin": "300-1800"}
        ]);
        let marginal = parent_count_marginal(&hist).expect("well-formed hist");
        assert_eq!(marginal[&19], vec![(5, 5)]);
        assert_eq!(marginal[&20], vec![(7, 1)]);
    }

    #[test]
    fn a_malformed_row_refuses_rather_than_being_skipped() {
        // Skipping would understate support, which is the direction that turns
        // an inadmissible cell admissible.
        let hist = json!([{"hour": 19, "count": 2}]);
        assert!(parent_count_marginal(&hist).is_err());
    }

    #[test]
    fn wasserstein_matches_a_hand_computed_distance() {
        // Two point masses: all weight at n = 0 against all weight at n = 3.
        // F - G is 1 across the whole interval, so the distance is exactly the
        // log1p gap, ln(4) - ln(1) = ln(4).
        let left = [(0_u32, 10_u64)];
        let right = [(3_u32, 7_u64)];
        let distance = wasserstein_log1p(&left, &right).expect("both populated");
        assert!((distance - 4_f64.ln()).abs() < 1e-12, "{distance}");
    }

    #[test]
    fn wasserstein_halves_when_half_the_mass_agrees() {
        // Half the mass sits on n = 0 for both sides; the other half is split
        // between 0 and 3. The CDF gap over [0, 3) is 1/2, so the distance is
        // half of ln(4).
        let left = [(0_u32, 2_u64)];
        let right = [(0_u32, 1_u64), (3_u32, 1_u64)];
        let distance = wasserstein_log1p(&left, &right).expect("both populated");
        assert!((distance - 4_f64.ln() / 2.0).abs() < 1e-12, "{distance}");
    }

    #[test]
    fn wasserstein_is_zero_for_identical_populations_and_symmetric() {
        let a = [(1_u32, 3_u64), (9_u32, 4_u64)];
        let b = [(1_u32, 6_u64), (9_u32, 8_u64)];
        // Same shape at twice the weight: a distance of zero, not a
        // weight-driven difference.
        assert_eq!(wasserstein_log1p(&a, &b), Some(0.0));
        let c = [(2_u32, 1_u64)];
        assert_eq!(wasserstein_log1p(&a, &c), wasserstein_log1p(&c, &a));
    }

    #[test]
    fn an_empty_side_is_undefined_rather_than_far() {
        assert_eq!(wasserstein_log1p(&[], &[]), Some(0.0));
        assert_eq!(wasserstein_log1p(&[(1, 1)], &[]), None);
        assert_eq!(wasserstein_log1p(&[], &[(1, 1)]), None);
    }

    #[test]
    fn the_frozen_grids_have_their_stated_point_counts() {
        // Section 16 states these counts explicitly, and two earlier revisions
        // of the spec got them wrong in opposite directions, so they are
        // pinned here against the rule rather than against a recollection.
        assert_eq!(log_grid(1e-6, 0.5, 3).len(), 19);
        assert_eq!(log_grid(10.0, 1000.0, 3).len(), 7);
        assert_eq!(log_grid(2.0, 200.0, 3).len(), 7);
        assert_eq!(log_grid(1.0, 3600.0, 3).len(), 12);
        assert_eq!(log_grid(2.0, 600.0, 3).len(), 9);
        assert_eq!(linear_grid(0.10, 0.60, 0.10).len(), 6);
        assert_eq!(linear_grid(0.2, 2.0, 0.2).len(), 10);
        assert_eq!(linear_grid(0.10, 0.85, 0.05).len(), 16);
    }

    #[test]
    fn the_shipped_switch_rate_lies_on_its_grid() {
        // The incumbent point's w = 0.10 is exactly 1e-6 * 10^(15/3), which is
        // why section 16 counts the reference cell once rather than as an
        // extra off-grid evaluation.
        let grid = log_grid(1e-6, 0.5, 3);
        assert!(
            grid.iter().any(|&w| (w - 0.10).abs() < 1e-12),
            "0.10 missing from {grid:?}"
        );
    }

    #[test]
    fn a_log_grid_appends_its_upper_endpoint_only_when_it_falls_short() {
        // 10 .. 1000 lands exactly on its endpoint at j = 6 and must not
        // duplicate it; 1e-6 .. 0.5 stops at 0.4641589 and must append.
        let exact = log_grid(10.0, 1000.0, 3);
        assert!((exact[exact.len() - 1] - 1000.0).abs() < 1e-9);
        assert!((exact[exact.len() - 2] - 1000.0).abs() > 1.0);
        let appended = log_grid(1e-6, 0.5, 3);
        assert!((appended[appended.len() - 1] - 0.5).abs() < 1e-12);
        assert!((appended[appended.len() - 2] - 0.464_158_883_361_278).abs() < 1e-9);
    }

    #[test]
    fn bins_name_their_edges_the_way_the_artifact_spells_them() {
        assert_eq!(bin_name(0), "0");
        assert_eq!(bin_name(1), "1-64");
        assert_eq!(bin_name(64), "1-64");
        assert_eq!(bin_name(65), "65-256");
        assert_eq!(bin_name(256), "65-256");
        assert_eq!(bin_name(257), "257-1024");
        assert_eq!(bin_name(1024), "257-1024");
        assert_eq!(bin_name(1025), "1025-4096");
        assert_eq!(bin_name(4096), "1025-4096");
        assert_eq!(bin_name(4097), "4097+");
    }
}
