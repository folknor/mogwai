# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Unit tests for the Binance probe modules that STAY Python.

TRIMMED at phase 4b item 7, and what was removed matters more than what is
left. This module used to carry 31 tests, 27 of which asserted behaviour inside
the ABSORB set - the level-visit estimator, the binning and quantiles, the
cadence mixture solve, the geometric sampler, the fingerprint's level queue. All
27 have verified Rust counterparts (`crates/mogwai-lab/src/characterize/tests.rs`,
`fingerprint.rs`, `cadence.rs`, `cadence_feasible.rs`), and their Python sources
have retired, so the tests retired with them rather than being pointed at code
that no longer exists.

What remains is the four tests over modules the triage kept Python -
`probe_binance_trades`, `probe_binance_klines` and `probe_binance_aggtrades` -
which are live intake helpers rather than absorbed method.

The trim was not optional. Every import at the top of this file was evaluated
at module load, so a single import of a retired module would have made the
WHOLE module unimportable and taken these four retained tests down with it -
a retained test suite silently failing to run because of what was removed
around it.

    python3 -m unittest analysis.test_characterize -v
"""

import tempfile
import unittest
import zipfile
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path

from analysis.probe_binance_aggtrades import probe as probe_aggtrades
from analysis.probe_binance_klines import probe as probe_klines
from analysis.probe_binance_trades import EventStats, probe as probe_trades


class ProbeTests(unittest.TestCase):
    """The three probes, plus the event-grouping distinction they rest on."""

    def test_event_grouping_rules_are_distinct(self):
        """Grouping by (timestamp, side) is not grouping by timestamp alone.

        Kept because it pins behaviour in `probe_binance_trades`, which stays
        Python. The Rust port of the same rule is separately covered by
        `cadence.rs::event_grouping_rules_are_distinct`.
        """
        side = EventStats(True)
        stamp = EventStats(False)
        for ts, taker_side, price in ((10, False, "1"), (10, True, "2"), (11, True, "2")):
            side.push(ts, taker_side, price)
            stamp.push(ts, taker_side, price)
        self.assertEqual(side.report()["events"], 3)
        self.assertEqual(stamp.report()["events"], 2)

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


if __name__ == "__main__":
    unittest.main()
