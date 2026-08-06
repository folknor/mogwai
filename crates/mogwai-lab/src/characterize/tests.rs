// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Unit fixtures mirroring `analysis/test_characterize.py`'s
//! `BinningTests`/`QuantileTests`/`VisitClosureTests`/`EraWindowTests`/
//! `ReportTests` - the reference coverage for the level-visit estimator,
//! since the Kraken corpus itself lives outside this repository (phase-3a
//! brief) and has no full-pass parity gate here.

use super::*;

const ERA: f64 = 1_000_000.0;

fn centre(value: f64) -> f64 {
    LVL_LOG_LO * 10f64.powf((lvl_bin(value) as f64 + 0.5) / LVL_PER_DEC)
}

fn visits(rows: &[(f64, f64, f64)]) -> LevelVisits {
    let mut acc = LevelVisits::new(ERA);
    for &(ts, px, sz) in rows {
        acc.push(ts, px, sz);
    }
    acc.close();
    acc
}

#[test]
fn bins_are_monotone_and_cover_the_support() {
    assert_eq!(lvl_bin(LVL_LOG_LO), 0);
    assert_eq!(lvl_bin(LVL_LOG_LO / 1000.0), 0);
    assert_eq!(lvl_bin(LVL_LOG_HI), LVL_BINS - 1);
    assert_eq!(lvl_bin(LVL_LOG_HI * 1000.0), LVL_BINS - 1);
    let mut previous: i64 = -1;
    for exponent in -6..=6 {
        for step in 0..(LVL_PER_DEC as i64) {
            let value = 10f64.powi(exponent) * 10f64.powf(step as f64 / LVL_PER_DEC);
            let current = lvl_bin(value) as i64;
            assert!(current >= previous);
            previous = current;
        }
    }
}

#[test]
fn half_and_five_land_in_different_bins() {
    assert!(lvl_bin(0.5) < lvl_bin(5.0));
    assert_eq!(lvl_bin(5.0) - lvl_bin(0.5), LVL_PER_DEC as usize);
    assert_eq!(lvl_bin(0.5) - lvl_bin(0.05), LVL_PER_DEC as usize);
}

#[test]
fn quantile_returns_the_geometric_bin_centre() {
    let mut hist = [0i64; LVL_BINS];
    hist[70] = 1; // bin 70 spans [1e-6 * 1e7, ...) = [10, 12.6)
    let q = histogram_quantile(&hist, 0.5).unwrap();
    assert!((q - LVL_LOG_LO * 10f64.powf(70.5 / LVL_PER_DEC)).abs() < 1e-12);
}

#[test]
fn quantile_picks_the_bin_holding_the_rank() {
    let mut hist = [0i64; LVL_BINS];
    hist[10] = 5;
    hist[20] = 5;
    assert_eq!(
        histogram_quantile(&hist, 0.5),
        histogram_quantile(&hist, 0.25)
    );
    assert!(histogram_quantile(&hist, 0.5).unwrap() < histogram_quantile(&hist, 0.9).unwrap());
    assert_eq!(
        histogram_quantile(&hist, 0.9),
        histogram_quantile(&hist, 1.0)
    );
}

#[test]
fn an_empty_histogram_has_no_quantile() {
    assert_eq!(histogram_quantile(&[0i64; LVL_BINS], 0.5), None);
}

#[test]
fn a_visit_closes_on_a_price_change_not_a_size_change() {
    let acc = visits(&[
        (ERA, 100.0, 1.0),
        (ERA + 1.0, 100.0, 7.0),
        (ERA + 2.0, 101.0, 1.0),
    ]);
    assert_eq!(acc.count, 2);
    assert_eq!(acc.n_hist[1], 1); // the two-print visit at 100.0
    assert_eq!(acc.n_hist[0], 1); // the one-print visit at 101.0
}

#[test]
fn equal_prices_at_non_adjacent_timestamps_are_one_visit() {
    let acc = visits(&[(ERA, 100.0, 1.0), (ERA + 9_999.0, 100.0, 1.0)]);
    assert_eq!(acc.count, 1);
    assert_eq!(acc.n_hist[1], 1);
}

#[test]
fn a_returning_price_opens_a_new_visit() {
    let acc = visits(&[
        (ERA, 100.0, 1.0),
        (ERA + 1.0, 101.0, 1.0),
        (ERA + 2.0, 100.0, 1.0),
    ]);
    assert_eq!(acc.count, 3);
    assert_eq!(acc.single, 3);
}

#[test]
fn single_print_visits_are_counted_and_multi_print_ones_are_not() {
    let acc = visits(&[
        (ERA, 100.0, 1.0),
        (ERA + 1.0, 101.0, 1.0),
        (ERA + 2.0, 101.0, 1.0),
    ]);
    assert_eq!(acc.count, 2);
    assert_eq!(acc.single, 1);
    assert!((acc.report()["single_print_frac"].as_f64().unwrap() - 0.5).abs() < 1e-12);
}

#[test]
fn the_final_open_visit_is_closed_and_binned() {
    let acc = visits(&[(ERA, 100.0, 2.0), (ERA + 1.0, 100.0, 3.0)]);
    assert_eq!(acc.count, 1);
    assert_eq!(acc.vol_hist[lvl_bin(5.0)], 1);
}

#[test]
fn closing_twice_bins_the_visit_once() {
    let mut acc = visits(&[(ERA, 100.0, 1.0)]);
    acc.close();
    assert_eq!(acc.count, 1);
}

#[test]
fn print_count_bins_group_eleven_to_twenty_and_twentyone_up() {
    assert_eq!([LevelVisits::n_bin(1), LevelVisits::n_bin(10)], [0, 9]);
    assert_eq!([LevelVisits::n_bin(11), LevelVisits::n_bin(20)], [10, 10]);
    assert_eq!(
        [LevelVisits::n_bin(21), LevelVisits::n_bin(5_000)],
        [11, 11]
    );
}

#[test]
fn a_visit_straddling_the_boundary_is_dropped_entirely() {
    let acc = visits(&[
        (ERA - 1.0, 100.0, 1.0),
        (ERA, 100.0, 1.0),
        (ERA + 1.0, 101.0, 1.0),
    ]);
    assert_eq!(acc.count, 1); // only the in-era visit at 101.0
    assert_eq!(acc.vol_hist.iter().sum::<i64>(), 1);
    assert_eq!(acc.vol_hist[lvl_bin(1.0)], 1);
}

#[test]
fn a_visit_opening_exactly_at_the_boundary_is_kept() {
    let acc = visits(&[(ERA, 100.0, 1.0)]);
    assert_eq!(acc.count, 1);
}

#[test]
fn a_visit_wholly_before_the_boundary_is_dropped() {
    let acc = visits(&[(ERA - 10.0, 100.0, 1.0), (ERA - 9.0, 101.0, 1.0)]);
    assert_eq!(acc.count, 0);
    assert!(acc.report()["single_print_frac"].is_null());
}

#[test]
fn the_normalizer_takes_only_in_era_sizes() {
    let acc = visits(&[(ERA - 1.0, 100.0, 8.0), (ERA + 1.0, 101.0, 2.0)]);
    assert_eq!(acc.size_hist.iter().sum::<i64>(), 1);
    assert_eq!(acc.size_hist[lvl_bin(2.0)], 1);
}

#[test]
fn the_normalizer_is_the_era_windowed_size_median() {
    let acc = visits(&[
        (ERA, 100.0, 2.0),
        (ERA + 1.0, 100.0, 2.0),
        (ERA + 2.0, 101.0, 2.0),
        (ERA + 3.0, 101.0, 2.0),
    ]);
    let report = acc.report();
    assert!((report["size_median"].as_f64().unwrap() - centre(2.0)).abs() < 1e-9);
    assert!((report["vol_p50_norm"].as_f64().unwrap() - centre(4.0) / centre(2.0)).abs() < 1e-9);
    assert!((report["vol_dispersion"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    assert!((report["size_dispersion"].as_f64().unwrap() - 1.0).abs() < 1e-9);
}

#[test]
fn dispersion_is_the_p90_over_p50_of_its_own_histogram() {
    let mut rows: Vec<(f64, f64, f64)> = (0..8)
        .map(|i| (ERA + i as f64, 100.0 + i as f64, 1.0))
        .collect();
    rows.push((ERA + 8.0, 200.0, 100.0));
    rows.push((ERA + 9.0, 201.0, 100.0));
    let report = visits(&rows).report();
    assert_eq!(report["n_visits"].as_i64().unwrap(), 10);
    assert!((report["single_print_frac"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    assert!(
        (report["vol_dispersion"].as_f64().unwrap() - centre(100.0) / centre(1.0)).abs() < 1e-6
    );
    assert!(report["vol_dispersion"].as_f64().unwrap() > 3.0);
}

#[test]
fn decimals_used_strips_trailing_zeros() {
    assert_eq!(decimals_used("1.2500"), 2);
    assert_eq!(decimals_used("1"), 0);
    assert_eq!(decimals_used("1.0"), 0);
    assert_eq!(decimals_used(" 1.230 "), 2);
}

#[test]
fn log_bin_clamps_at_the_ends_and_is_monotone() {
    assert_eq!(log_bin(0.0, 1e-3, 86400.0, 40), 0);
    assert_eq!(log_bin(1e9, 1e-3, 86400.0, 40), 39);
    assert!(log_bin(1.0, 1e-3, 86400.0, 40) < log_bin(100.0, 1e-3, 86400.0, 40));
}
