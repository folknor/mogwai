# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Unit tests for the at-touch level-visit estimator.

The proceed/close verdict on a trades-only queue-ahead fill model is read off
one number this code computes, so a corpus run alone is not evidence: it answers
"what number came out", never "is this the intended computation". These run
against a handful of synthetic rows and pin the boundary-sensitive parts - run
closure, the era window, the binning, and the histogram quantiles.
"""

import unittest
import tempfile
import zipfile
from pathlib import Path
from contextlib import redirect_stdout
from io import StringIO

from analysis.build_fingerprint import level_queue, level_verdict, load_cadence
from analysis.characterize import (
    LVL_BINS,
    LVL_LOG_HI,
    LVL_LOG_LO,
    LVL_PER_DEC,
    LevelVisits,
    histogram_quantile,
    lvl_bin,
)
from analysis.build_cadence import solve_shape
from analysis.check_cadence_feasible import next_count
from analysis.probe_binance_aggtrades import AutoCorr
from analysis.probe_binance_aggtrades import probe as probe_aggtrades
from analysis.probe_binance_klines import probe as probe_klines
from analysis.probe_binance_trades import EventStats, probe as probe_trades

ERA = 1_000_000


def centre(value):
    """The bin centre a quantile lands on once `value` selects its bin."""
    return LVL_LOG_LO * 10 ** ((lvl_bin(value) + 0.5) / LVL_PER_DEC)


def visits(rows, era_start_ts=ERA):
    acc = LevelVisits(era_start_ts=era_start_ts)
    for ts, px, sz in rows:
        acc.push(ts, px, sz)
    acc.close()
    return acc


class BinningTests(unittest.TestCase):
    def test_bins_are_monotone_and_cover_the_support(self):
        self.assertEqual(lvl_bin(LVL_LOG_LO), 0)
        self.assertEqual(lvl_bin(LVL_LOG_LO / 1000), 0)
        self.assertEqual(lvl_bin(LVL_LOG_HI), LVL_BINS - 1)
        self.assertEqual(lvl_bin(LVL_LOG_HI * 1000), LVL_BINS - 1)
        previous = -1
        for exponent in range(-6, 7):
            for step in range(LVL_PER_DEC):
                current = lvl_bin(10.0 ** exponent * 10.0 ** (step / LVL_PER_DEC))
                self.assertGreaterEqual(current, previous)
                previous = current

    def test_half_and_five_land_in_different_bins(self):
        # Regression on the size_log_hist binning this one replaces: int()
        # truncates toward zero, so there 0.5 and 5.0 shared a bin while 0.05
        # and 0.5 did not. floor() over a shifted support is regular around 1.
        self.assertLess(lvl_bin(0.5), lvl_bin(5.0))
        self.assertEqual(lvl_bin(5.0) - lvl_bin(0.5), LVL_PER_DEC)
        self.assertEqual(lvl_bin(0.5) - lvl_bin(0.05), LVL_PER_DEC)


class QuantileTests(unittest.TestCase):
    def test_quantile_returns_the_geometric_bin_centre(self):
        hist = [0] * LVL_BINS
        hist[70] = 1  # bin 70 spans [1e-6 * 1e7, ...) = [10, 12.6)
        self.assertAlmostEqual(
            histogram_quantile(hist, 0.5),
            LVL_LOG_LO * 10 ** (70.5 / LVL_PER_DEC),
        )

    def test_quantile_picks_the_bin_holding_the_rank(self):
        hist = [0] * LVL_BINS
        hist[10] = 5
        hist[20] = 5
        self.assertEqual(histogram_quantile(hist, 0.5), histogram_quantile(hist, 0.25))
        self.assertLess(histogram_quantile(hist, 0.5), histogram_quantile(hist, 0.9))
        self.assertEqual(histogram_quantile(hist, 0.9), histogram_quantile(hist, 1.0))

    def test_an_empty_histogram_has_no_quantile(self):
        self.assertIsNone(histogram_quantile([0] * LVL_BINS, 0.5))


class VisitClosureTests(unittest.TestCase):
    def test_a_visit_closes_on_a_price_change_not_a_size_change(self):
        acc = visits([(ERA, 100.0, 1.0), (ERA + 1, 100.0, 7.0), (ERA + 2, 101.0, 1.0)])
        self.assertEqual(acc.count, 2)
        self.assertEqual(acc.n_hist[1], 1)  # the two-print visit at 100.0
        self.assertEqual(acc.n_hist[0], 1)  # the one-print visit at 101.0

    def test_equal_prices_at_non_adjacent_timestamps_are_one_visit(self):
        acc = visits([(ERA, 100.0, 1.0), (ERA + 9_999, 100.0, 1.0)])
        self.assertEqual(acc.count, 1)
        self.assertEqual(acc.n_hist[1], 1)

    def test_a_returning_price_opens_a_new_visit(self):
        acc = visits([
            (ERA, 100.0, 1.0), (ERA + 1, 101.0, 1.0), (ERA + 2, 100.0, 1.0)])
        self.assertEqual(acc.count, 3)
        self.assertEqual(acc.single, 3)

    def test_single_print_visits_are_counted_and_multi_print_ones_are_not(self):
        acc = visits([
            (ERA, 100.0, 1.0),
            (ERA + 1, 101.0, 1.0), (ERA + 2, 101.0, 1.0),
        ])
        self.assertEqual(acc.count, 2)
        self.assertEqual(acc.single, 1)
        self.assertAlmostEqual(acc.report()["single_print_frac"], 0.5)

    def test_the_final_open_visit_is_closed_and_binned(self):
        acc = visits([(ERA, 100.0, 2.0), (ERA + 1, 100.0, 3.0)])
        self.assertEqual(acc.count, 1)
        self.assertEqual(acc.vol_hist[lvl_bin(5.0)], 1)

    def test_closing_twice_bins_the_visit_once(self):
        acc = visits([(ERA, 100.0, 1.0)])
        acc.close()
        self.assertEqual(acc.count, 1)

    def test_print_count_bins_group_eleven_to_twenty_and_twentyone_up(self):
        self.assertEqual([LevelVisits.n_bin(n) for n in (1, 10)], [0, 9])
        self.assertEqual([LevelVisits.n_bin(n) for n in (11, 20)], [10, 10])
        self.assertEqual([LevelVisits.n_bin(n) for n in (21, 5_000)], [11, 11])


class EraWindowTests(unittest.TestCase):
    def test_a_visit_straddling_the_boundary_is_dropped_entirely(self):
        acc = visits([(ERA - 1, 100.0, 1.0), (ERA, 100.0, 1.0), (ERA + 1, 101.0, 1.0)])
        self.assertEqual(acc.count, 1)  # only the in-era visit at 101.0
        self.assertEqual(sum(acc.vol_hist), 1)
        self.assertEqual(acc.vol_hist[lvl_bin(1.0)], 1)

    def test_a_visit_opening_exactly_at_the_boundary_is_kept(self):
        acc = visits([(ERA, 100.0, 1.0)])
        self.assertEqual(acc.count, 1)

    def test_a_visit_wholly_before_the_boundary_is_dropped(self):
        acc = visits([(ERA - 10, 100.0, 1.0), (ERA - 9, 101.0, 1.0)])
        self.assertEqual(acc.count, 0)
        self.assertIsNone(acc.report()["single_print_frac"])

    def test_the_normalizer_takes_only_in_era_sizes(self):
        acc = visits([(ERA - 1, 100.0, 8.0), (ERA + 1, 101.0, 2.0)])
        self.assertEqual(sum(acc.size_hist), 1)
        self.assertEqual(acc.size_hist[lvl_bin(2.0)], 1)


class ReportTests(unittest.TestCase):
    def test_the_normalizer_is_the_era_windowed_size_median(self):
        # Two visits of 4.0, every trade size 2.0: the median trade size is
        # 2.0, so a p50 visit volume of 4.0 normalizes to 2.0 typical trades.
        acc = visits([
            (ERA, 100.0, 2.0), (ERA + 1, 100.0, 2.0),
            (ERA + 2, 101.0, 2.0), (ERA + 3, 101.0, 2.0),
        ])
        report = acc.report()
        # Quantiles are bin centres, so the reading carries the binning's ~26
        # percent resolution rather than the exact input value.
        self.assertAlmostEqual(report["size_median"], centre(2.0))
        self.assertAlmostEqual(report["vol_p50_norm"], centre(4.0) / centre(2.0))
        self.assertAlmostEqual(report["vol_dispersion"], 1.0)
        self.assertAlmostEqual(report["size_dispersion"], 1.0)

    def test_dispersion_is_the_p90_over_p50_of_its_own_histogram(self):
        # Eight one-unit visits and two hundred-unit ones: the rank-9 p90 is an
        # outlier visit, the rank-5 p50 is not, so the ratio is ~100.
        rows = [(ERA + i, 100.0 + i, 1.0) for i in range(8)]
        rows.append((ERA + 8, 200.0, 100.0))
        rows.append((ERA + 9, 201.0, 100.0))
        report = visits(rows).report()
        self.assertEqual(report["n_visits"], 10)
        self.assertAlmostEqual(report["single_print_frac"], 1.0)
        self.assertAlmostEqual(report["vol_dispersion"], centre(100.0) / centre(1.0))
        self.assertGreater(report["vol_dispersion"], 3.0)


class LevelQueueTests(unittest.TestCase):
    def fixture(self, single_print_frac=0.2, vol_dispersion=5.0):
        report = visits([
            (ERA, 100.0, 2.0), (ERA + 1, 100.0, 2.0),
            (ERA + 2, 101.0, 2.0), (ERA + 3, 101.0, 6.0),
        ]).report()
        report["single_print_frac"] = single_print_frac
        report["vol_dispersion"] = vol_dispersion
        report["size_dispersion"] = 1.0
        return {"XBTUSD": {"level": report}}

    def test_support_is_increasing_and_the_pmf_sums_to_one(self):
        reports = self.fixture()
        block = level_queue(reports["XBTUSD"], reports)
        self.assertEqual(len(block["support_norm"]), len(block["pmf"]))
        self.assertEqual(len(block["pmf"]), LVL_BINS)
        self.assertAlmostEqual(sum(block["pmf"]), 1.0)
        self.assertTrue(all(x > 0 for x in block["support_norm"]))
        self.assertTrue(all(
            a < b for a, b in zip(block["support_norm"], block["support_norm"][1:])))

    def test_a_report_predating_the_measurement_is_refused(self):
        reports = self.fixture()
        reports["ETHUSD"] = {"session": {}}
        with self.assertRaises(ValueError) as caught:
            level_queue(reports["XBTUSD"], reports)
        self.assertIn("ETHUSD", str(caught.exception))

    def test_disagreeing_binning_is_refused(self):
        reports = self.fixture()
        other = dict(reports["XBTUSD"]["level"], bins_per_decade=20)
        reports["ETHUSD"] = {"level": other}
        with self.assertRaises(ValueError):
            level_queue(reports["XBTUSD"], reports)

    def test_all_four_conditions_holding_proceeds(self):
        level = self.fixture()["XBTUSD"]["level"]
        verdict = level_verdict(level, [0.2, 0.25])
        self.assertTrue(verdict["proceed"])
        self.assertEqual(verdict["failed"], [])

    def test_a_degenerate_corpus_declines_and_names_the_failed_condition(self):
        level = self.fixture(single_print_frac=0.69)["XBTUSD"]["level"]
        verdict = level_verdict(level, [0.69, 0.7])
        self.assertFalse(verdict["proceed"])
        self.assertEqual(verdict["failed"], ["single_print_frac <= 0.50"])

    def test_inherited_dispersion_fails_condition_three(self):
        level = self.fixture(vol_dispersion=4.0)["XBTUSD"]["level"]
        level["size_dispersion"] = 4.0
        verdict = level_verdict(level, [0.2])
        self.assertFalse(verdict["proceed"])
        self.assertIn("vol_dispersion >= 1.5 * size_dispersion", verdict["failed"])

    def test_an_outlier_anchor_fails_condition_four(self):
        level = self.fixture(single_print_frac=0.05)["XBTUSD"]["level"]
        verdict = level_verdict(level, [0.05, 0.4, 0.45])
        self.assertFalse(verdict["proceed"])
        self.assertIn(
            "anchor single_print_frac within 1.5x of the cross-pair median",
            verdict["failed"])
        self.assertAlmostEqual(verdict["single_print_frac_cross_pair_median"], 0.4)


class CadenceTests(unittest.TestCase):
    def test_event_grouping_rules_are_distinct(self):
        side = EventStats(True)
        stamp = EventStats(False)
        for ts, taker_side, price in ((10, False, "1"), (10, True, "2"), (11, True, "2")):
            side.push(ts, taker_side, price)
            stamp.push(ts, taker_side, price)
        self.assertEqual(side.report()["events"], 3)
        self.assertEqual(stamp.report()["events"], 2)

    def test_mixture_solution_and_fallback(self):
        mixed = solve_shape(5.0, 0.5, 2.0)
        self.assertFalse(mixed["fallback_pure_geometric"])
        self.assertAlmostEqual(mixed["m"], 8.0)
        fallback = solve_shape(5.0, 0.1, 2.0)
        self.assertTrue(fallback["fallback_pure_geometric"])
        self.assertEqual(fallback["q"], 0.0)
        self.assertEqual(fallback["m"], 5.0)

    def test_geometric_sampler_uses_the_pinned_inverse_cdf(self):
        values = iter((0.9, 0.0, 0.9, 0.5))
        uniform = lambda: next(values)
        self.assertEqual(next_count(uniform, 0.5, 4.0)[0], 1)
        self.assertEqual(next_count(uniform, 0.5, 4.0)[0], 3)

    def test_raw_probe_returns_structured_result(self):
        with tempfile.TemporaryDirectory(dir=Path(__file__).parent) as directory:
            path = Path(directory) / "fixture.zip"
            rows = (
                "1,100,1,100,1000000,false,true\n"
                "2,100,2,200,1000000,false,true\n"
                "3,101,1,101,2000000,true,true\n"
            )
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("fixture.csv", rows)
            with redirect_stdout(StringIO()):
                result = probe_trades(path)
        self.assertEqual(result["rows"], 3)
        self.assertEqual(result["timestamp_and_side"]["events"], 2)
        self.assertIn("per_second_counts", result)

    def test_kline_probe_returns_structured_result(self):
        with tempfile.TemporaryDirectory(dir=Path(__file__).parent) as directory:
            path = Path(directory) / "fixture.zip"
            rows = (
                "1000,1,1,1,1,2,1999,20,2,1,10,0\n"
                "2000,1,1,1,1,3,2999,30,0,2,20,0\n"
            )
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("fixture.csv", rows)
            with redirect_stdout(StringIO()):
                result = probe_klines(path)
        self.assertEqual(result["raw_trades"], 2)
        self.assertEqual(result["per_second_counts"]["median"], 1.0)

    def test_aggtrades_probe_returns_structured_result(self):
        with tempfile.TemporaryDirectory(dir=Path(__file__).parent) as directory:
            path = Path(directory) / "fixture.zip"
            rows = "".join(
                f"{i},100,1,{i},{i},{1_000_000 + i * 1_000_000},false,true\n"
                for i in range(8)
            )
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("fixture.csv", rows)
            with redirect_stdout(StringIO()):
                result = probe_aggtrades(path)
        self.assertEqual(result["trades"], 8)
        self.assertIn("event_duration", result)

    def test_committed_cadence_is_loadable(self):
        self.assertEqual(load_cadence()["anchor"], "BTCUSDT")


if __name__ == "__main__":
    unittest.main()
