#!/usr/bin/env python3
"""Smoke check for the two probe entry points over tiny synthetic archives.

The conformance and equivalence gates exercise `build_targets.compute_targets`
but not `probe_binance_aggtrades.probe` or `probe_binance_trades.probe`, whose
parse loops were rewritten onto the binary `_byte_lines` path. This builds a
handful of hand-auditable rows in each layout, runs both probes for real
(zip member and all), and asserts the countable results.

Usage:
    python3 -u analysis/probe_smoke.py <scratch-dir>
"""

from __future__ import annotations

import sys
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from probe_binance_aggtrades import probe as probe_agg  # noqa: E402
from probe_binance_trades import probe as probe_trades  # noqa: E402


def close(a, b, tol=1e-12):
    return abs(a - b) <= tol * max(1.0, abs(a), abs(b))


def main() -> int:
    scratch = Path(sys.argv[1])
    scratch.mkdir(parents=True, exist_ok=True)
    failures = 0

    def check(name, ok):
        nonlocal failures
        if not ok:
            failures += 1
        print(f"  {'ok  ' if ok else 'FAIL'}  {name}")

    # aggTrades layout: agg_id, price, qty, first_id, last_id, time, maker.
    # Three distinct stamps 1s apart; the middle one is a 2-print sweep, and
    # the final sweep (1 print) must be counted - the pre-fix loop dropped it.
    # Eight events one second apart (enough gaps for the lag-5 ACF print);
    # the second event is a 2-print sweep, every other sweep is a single.
    agg_rows = [
        "1,100.0,1.0,1,1,1000000,True",
        "2,101.0,2.0,2,2,2000000,False",
        "3,102.0,4.0,3,3,2000000,True",
        "4,103.0,8.0,4,4,3000000,False",
        "5,104.0,1.0,5,5,4000000,False",
        "6,105.0,1.0,6,6,5000000,False",
        "7,106.0,1.0,7,7,6000000,False",
        "8,107.0,1.0,8,8,7000000,False",
        "9,108.0,1.0,9,9,8000000,False",
    ]
    agg_zip = scratch / "smoke-aggtrades.zip"
    with zipfile.ZipFile(agg_zip, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("smoke-aggtrades.csv", "\r\n".join(agg_rows) + "\r\n")
    r = probe_agg(str(agg_zip))
    check("agg trades count", r["trades"] == 9)
    check("agg events count", r["events"] == 8)
    # Sweeps are 1,2,1,1,1,1,1,1 across the eight events: mean 9/8, max 2. The
    # final single-print sweep must be counted - the pre-fix loop dropped it.
    check("agg children_mean covers final sweep", close(r["children_mean"], 9 / 8))
    check("agg children_max", r["children_max"] == 2)
    check("agg mean size", close(r["mean_trade_size"], 20.0 / 9))
    # Maker rows carry qty 1 and 4 of 20 total, taker-buy share = 15/20.
    check("agg taker share", close(r["taker_buy_share"], 0.75))
    check("agg event gap mean 1s", close(r["event_duration"]["mean_s"], 1.0))

    # trades layout: id, price, qty, quote_qty, time, is_buyer_maker.
    # Stamps: two rows share 1_000_000 with the same side, then 2_000_000 with
    # opposite sides, then 3_000_000: timestamp+side gives 4 parents,
    # timestamp-only gives 3. Header row must be skipped.
    trade_rows = [
        "id,price,qty,quote_qty,time,is_buyer_maker",
        "1,100.0,1.0,100.0,1000000,True",
        "2,100.5,1.0,100.5,1000000,True",
        "3,101.0,1.0,101.0,2000000,True",
        "4,101.5,1.0,101.5,2000000,False",
        "5,102.0,1.0,102.0,3000000,True",
    ]
    trades_zip = scratch / "smoke-trades.zip"
    with zipfile.ZipFile(trades_zip, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("smoke-trades.csv", "\r\n".join(trade_rows) + "\r\n")
    r = probe_trades(str(trades_zip))
    check("trades rows", r["rows"] == 5)
    check("trades side parents", r["timestamp_and_side"]["events"] == 4)
    check("trades time parents", r["timestamp_only"]["events"] == 3)
    check("trades side children mean", close(r["timestamp_and_side"]["children"]["mean"], 5 / 4))
    # Each parent occupies a distinct price level except the first (2 levels).
    check("trades side levels mean", close(r["timestamp_and_side"]["levels"]["mean"], 5 / 4))
    check("trades notional", close(r["mean_notional"], 505.0 / 5))

    print()
    print("SMOKE PASS" if not failures else f"SMOKE FAILED: {failures}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
