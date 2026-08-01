#!/usr/bin/env python3
"""Render a `mogwai gen` CSV as an interactive candlestick / tick chart.

Reads either shape the generator emits and writes ONE self-contained HTML file
that pulls TradingView's lightweight-charts from a CDN:

  bars   header `open_ts,close_ts,open,high,low,close,volume,trade_count`
  trades header `ts_event,price,size,aggressor`

The shape is detected from the header, not from a flag, so `--type` never has
to be repeated here. Stdlib only, matching the rest of `analysis/`: no
matplotlib, no plotly, no venv, nothing to install.

Two ways in:

  # chart a CSV you already dumped
  python3 analysis/plot_tape.py --csv bars.csv --open

  # dump and chart in one step (shells out to `brokkr run mogwai -- gen`)
  python3 analysis/plot_tape.py --gen --type bars --interval 1m --length 1d --open

Everything after `--gen` that this script does not consume itself is forwarded
to `mogwai gen` verbatim, so `--regime`, `--havoc`, `--seed`, `--start-price`
and friends work without being mirrored here.

A note on the time axis. lightweight-charts keys points by INTEGER UNIX
SECONDS, strictly ascending and unique, but the generator stamps nanoseconds
and emits many trades inside one second. Bars are therefore charted exactly as
generated (any interval >= 1s is already unique), while a trades CSV is folded
to one point per second: price is the last print in that second, and volume is
split into buyer- and seller-aggressed halves. Deserts survive the fold - a
second with no prints produces no point - which is the property that makes this
worth looking at under a LiquidityDrought.

The generator's default `--start-ts 0` puts the tape at the 1970 epoch. That is
the canonical anchor-independent tape, not a bug, and it charts fine; pass
`--start-ts` through to `gen` if you would rather read wall-clock dates.
"""

from __future__ import annotations

import argparse
import csv
import html
import json
import os
import shutil
import subprocess
import sys
import webbrowser

NS_PER_SEC = 1_000_000_000

BARS_HEADER = [
    "open_ts",
    "close_ts",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "trade_count",
]
TRADES_HEADER = ["ts_event", "price", "size", "aggressor"]

# Volume-pane colors. Buyer/seller split the trades view; bars use the up/down
# of the candle itself. `EMPTY` marks a carried-forward zero-volume bar - the
# generator's desert fill - so a drought reads as grey flatline rather than
# silently looking like a quiet-but-real stretch of tape.
UP = "#26a69a"
DOWN = "#ef5350"
EMPTY = "#6b7280"

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_CSV = os.path.join(REPO_ROOT, "analysis", "out", "gen.csv")
DEFAULT_HTML = os.path.join(REPO_ROOT, "analysis", "out", "chart.html")


def parse_args(argv: list[str]) -> tuple[argparse.Namespace, list[str]]:
    p = argparse.ArgumentParser(
        prog="plot_tape.py",
        description="Chart a mogwai gen CSV as interactive candles or ticks.",
        epilog="Unrecognized flags are forwarded to `mogwai gen` under --gen.",
    )
    src = p.add_argument_group("input")
    src.add_argument(
        "--csv",
        metavar="PATH",
        help="CSV to chart. Under --gen this is where the dump is written "
        f"(default {os.path.relpath(DEFAULT_CSV, REPO_ROOT)}).",
    )
    src.add_argument(
        "--gen",
        action="store_true",
        help="Run `brokkr run mogwai -- gen` first, then chart its output.",
    )
    out = p.add_argument_group("output")
    out.add_argument(
        "--out",
        metavar="PATH",
        default=DEFAULT_HTML,
        help=f"HTML to write (default {os.path.relpath(DEFAULT_HTML, REPO_ROOT)}).",
    )
    out.add_argument(
        "--title",
        metavar="TEXT",
        help="Chart heading. Defaults to the CSV name plus the row count.",
    )
    out.add_argument(
        "--open",
        dest="open_browser",
        action="store_true",
        help="Open the written HTML in the default browser.",
    )
    return p.parse_known_args(argv)


def run_gen(csv_path: str, forwarded: list[str]) -> None:
    """Dump a tape to `csv_path` via brokkr, forwarding `forwarded` to gen.

    `--out` is appended last and is this script's to own: the CSV path comes
    from `--csv`, so a caller passing their own `--out` through would be
    writing somewhere we then fail to read. Reject that pairing explicitly
    instead of letting clap take the last one and leaving us confused.
    """
    if "--out" in forwarded:
        sys.exit("plot_tape: pass --csv, not --out, to choose the dump path")
    if shutil.which("brokkr") is None:
        sys.exit("plot_tape: brokkr not on PATH; dump the CSV yourself and use --csv")
    os.makedirs(os.path.dirname(csv_path) or ".", exist_ok=True)
    cmd = ["brokkr", "run", "mogwai", "--", "gen"] + forwarded + ["--out", csv_path]
    print("plot_tape: " + " ".join(cmd), file=sys.stderr)
    done = subprocess.run(cmd, cwd=REPO_ROOT)
    if done.returncode != 0:
        sys.exit(f"plot_tape: gen failed with status {done.returncode}")


def read_rows(csv_path: str) -> tuple[str, list[dict[str, str]]]:
    """Return (`kind`, rows) for a gen CSV, kind being "bars" or "trades"."""
    try:
        handle = open(csv_path, newline="")
    except OSError as err:
        sys.exit(f"plot_tape: cannot read {csv_path}: {err}")
    with handle:
        reader = csv.DictReader(handle)
        header = reader.fieldnames
        if header == BARS_HEADER:
            kind = "bars"
        elif header == TRADES_HEADER:
            kind = "trades"
        else:
            sys.exit(
                f"plot_tape: {csv_path} is not a gen CSV; header was {header}, "
                f"expected {BARS_HEADER} or {TRADES_HEADER}"
            )
        return kind, list(reader)


def bar_series(rows: list[dict[str, str]]) -> tuple[list[dict], list[dict]]:
    """Candles plus a volume histogram, keyed on each window's OPEN.

    Keying on the open is what puts a candle at the left edge of the window it
    summarizes; keying on the close would shift the whole tape right by one
    interval. Duplicate seconds cannot occur here because gen's grid is
    epoch-anchored at the requested interval, but sub-second intervals would
    collide, so those are dropped with a warning rather than handed to
    lightweight-charts, which rejects a non-ascending series outright.
    """
    candles: list[dict] = []
    volumes: list[dict] = []
    last_t: int | None = None
    collisions = 0
    for row in rows:
        t = int(row["open_ts"]) // NS_PER_SEC
        if last_t is not None and t <= last_t:
            collisions += 1
            continue
        last_t = t
        o, h, low, c = (
            float(row["open"]),
            float(row["high"]),
            float(row["low"]),
            float(row["close"]),
        )
        candles.append({"time": t, "open": o, "high": h, "low": low, "close": c})
        empty = int(row["trade_count"]) == 0
        volumes.append(
            {
                "time": t,
                "value": float(row["volume"]),
                "color": EMPTY if empty else (UP if c >= o else DOWN),
            }
        )
    if collisions:
        print(
            f"plot_tape: dropped {collisions} bars sharing a second with the "
            "previous one; the chart axis is whole seconds, so use an "
            "--interval of 1s or more to see every bar",
            file=sys.stderr,
        )
    return candles, volumes


def trade_series(rows: list[dict[str, str]]) -> tuple[list[dict], list[dict], list[dict]]:
    """Fold ticks to one point per second: price line, buy volume, sell volume.

    Price is the LAST print in the second (the close, consistent with how the
    bar path renders). Volume is split by aggressor and the seller half is
    returned NEGATED, which is what makes the chart draw it downward as a
    mirror of the buyer half rather than stacking two bars on one baseline
    where the shorter vanishes behind the taller. A second with no prints
    yields no point at all, so droughts stay holes.
    """
    price: dict[int, float] = {}
    buys: dict[int, float] = {}
    sells: dict[int, float] = {}
    for row in rows:
        t = int(row["ts_event"]) // NS_PER_SEC
        price[t] = float(row["price"])
        size = float(row["size"])
        side = row["aggressor"]
        if side == "buyer":
            buys[t] = buys.get(t, 0.0) + size
        elif side == "seller":
            sells[t] = sells.get(t, 0.0) + size
        # `none` contributes to neither half; it still moved the price line.
    times = sorted(price)
    line = [{"time": t, "value": price[t]} for t in times]
    buy_bars = [{"time": t, "value": buys[t]} for t in times if t in buys]
    sell_bars = [{"time": t, "value": -sells[t]} for t in times if t in sells]
    return line, buy_bars, sell_bars


TEMPLATE = """<!DOCTYPE html>
<meta charset="utf-8">
<title>__TITLE__</title>
<script src="https://unpkg.com/lightweight-charts@4.1.3/dist/lightweight-charts.standalone.production.js"></script>
<style>
  html, body { margin: 0; height: 100%; background: #131722; color: #d1d4dc;
    font: 13px system-ui, sans-serif; }
  header { padding: 10px 14px; border-bottom: 1px solid #2a2e39; }
  h1 { margin: 0; font-size: 14px; font-weight: 600; }
  #meta { margin-top: 3px; color: #787b86; font-size: 12px; }
  #chart { position: absolute; top: 58px; left: 0; right: 0; bottom: 0; }
</style>
<header><h1>__TITLE__</h1><div id="meta">__META__</div></header>
<div id="chart"></div>
<script>
const DATA = __DATA__;
const el = document.getElementById('chart');
const chart = LightweightCharts.createChart(el, {
  autoSize: true,
  layout: { background: { color: '#131722' }, textColor: '#d1d4dc' },
  grid: { vertLines: { color: '#1f2430' }, horzLines: { color: '#1f2430' } },
  rightPriceScale: { borderColor: '#2a2e39', scaleMargins: { top: 0.06, bottom: 0.28 } },
  timeScale: { borderColor: '#2a2e39', timeVisible: true, secondsVisible: true },
  crosshair: { mode: LightweightCharts.CrosshairMode.Normal },
});

// The volume pane is an overlay on its own scale, pinned to the lower quarter.
// Two histograms share that scale in the trades view; sell sizes arrive
// negated, so the seller half hangs below the zero line instead of hiding
// behind the buyer half - histogram series draw from a common baseline and
// would otherwise simply occlude each other.
function volumeScale(id) {
  chart.priceScale(id).applyOptions({ scaleMargins: { top: 0.78, bottom: 0 } });
}
function histogram(id, color) {
  const s = chart.addHistogramSeries({
    priceFormat: { type: 'volume' }, priceScaleId: id, color,
  });
  volumeScale(id);
  return s;
}

if (DATA.kind === 'bars') {
  const candles = chart.addCandlestickSeries({
    upColor: '__UP__', downColor: '__DOWN__',
    borderUpColor: '__UP__', borderDownColor: '__DOWN__',
    wickUpColor: '__UP__', wickDownColor: '__DOWN__',
  });
  candles.setData(DATA.candles);
  histogram('vol', '__UP__').setData(DATA.volumes);
} else {
  const line = chart.addLineSeries({ color: '#d1d4dc', lineWidth: 1 });
  line.setData(DATA.line);
  histogram('vol', '__UP__').setData(DATA.buys);
  histogram('vol', '__DOWN__').setData(DATA.sells);
}
chart.timeScale().fitContent();
</script>
"""


def render(title: str, meta: str, payload: dict) -> str:
    page = TEMPLATE
    for token, value in (
        ("__TITLE__", html.escape(title)),
        ("__META__", html.escape(meta)),
        ("__UP__", UP),
        ("__DOWN__", DOWN),
    ):
        page = page.replace(token, value)
    # Data goes in last: it is JSON, so it can contain the literal token text
    # of nothing above, but substituting it first would let a stray `__UP__`
    # inside a filename-derived string get rewritten.
    return page.replace("__DATA__", json.dumps(payload, separators=(",", ":")))


def main(argv: list[str]) -> None:
    args, forwarded = parse_args(argv)
    csv_path = args.csv or DEFAULT_CSV
    if args.gen:
        run_gen(csv_path, forwarded)
    elif forwarded:
        sys.exit(f"plot_tape: unknown arguments {forwarded} (did you mean --gen?)")

    kind, rows = read_rows(csv_path)
    if not rows:
        sys.exit(f"plot_tape: {csv_path} has a header but no rows")

    if kind == "bars":
        candles, volumes = bar_series(rows)
        payload = {"kind": "bars", "candles": candles, "volumes": volumes}
        empties = sum(1 for v in volumes if v["color"] == EMPTY)
        meta = f"{len(candles)} bars, {empties} of them empty (zero-trade windows)"
    else:
        line, buys, sells = trade_series(rows)
        payload = {"kind": "trades", "line": line, "buys": buys, "sells": sells}
        meta = f"{len(rows)} trades folded to {len(line)} one-second points"

    title = args.title or f"{os.path.basename(csv_path)} ({kind})"
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    with open(args.out, "w") as f:
        f.write(render(title, meta, payload))
    print(f"plot_tape: wrote {args.out} - {meta}", file=sys.stderr)
    if args.open_browser:
        webbrowser.open("file://" + os.path.abspath(args.out))


if __name__ == "__main__":
    main(sys.argv[1:])
