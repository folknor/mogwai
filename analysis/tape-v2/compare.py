#!/usr/bin/env python3
"""Stack several bars CSVs on one page with linked time axes.

The chart gate is one CSV per page, which is right for looking at one tape
and wrong for comparing two: the eye cannot hold a real week in one tab and
a generated week in another. This renders N bars CSVs as N candlestick
panes, one above the other, with every pane's visible range following
whichever one is scrolled or zoomed. Stdlib only, like `plot_tape.py`,
whose series folding it reuses.

    python3 analysis/tape-v2/compare.py --out page.html \\
        "Real ES=analysis/out/e0/real-ES.csv" \\
        "Real MNQ=analysis/out/e0/real-MNQ.csv" \\
        "Gen MNQ=analysis/out/e0/gen-MNQ.csv"

The time axis is UTC. Each pane's price scale is its own; the point of the
page is texture, ignition, gaps and volume shape, not level. Because the
scales differ, each label carries the pane's week range in points: a pane
that travels twice as far is drawn at half the magnification, and a bar of
the same height in two panes is not the same move.

Every pane is drawn on one shared minute grid, every minute from the first
traded one to the last. A minute a pane has no bar for is a whitespace
point, so a closure is an hour of blank space in every pane and a reopen
gap is seen across it, and the same screen position is the same minute in
every pane. Without this the panes drift apart by the length of every
closure, or a closure vanishes and its gap reads as a jump between
neighbouring bars.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ANALYSIS = os.path.dirname(HERE)
sys.path.insert(0, ANALYSIS)

from plot_tape import DOWN, EMPTY, UP, bar_series, read_rows  # noqa: E402

TEMPLATE = """<!DOCTYPE html>
<meta charset="utf-8">
<title>__TITLE__</title>
<script src="https://unpkg.com/lightweight-charts@4.1.3/dist/lightweight-charts.standalone.production.js"></script>
<style>
  html, body { margin: 0; height: 100%; background: #131722; color: #d1d4dc;
    font: 13px system-ui, sans-serif; }
  header { padding: 8px 14px; border-bottom: 1px solid #2a2e39; }
  h1 { margin: 0; font-size: 14px; font-weight: 600; }
  #meta { margin-top: 3px; color: #787b86; font-size: 12px; }
  #panes { position: absolute; top: 50px; left: 0; right: 0; bottom: 0;
    display: flex; flex-direction: column; }
  .pane { flex: 1 1 0; position: relative; border-top: 1px solid #2a2e39; }
  .label { position: absolute; top: 6px; left: 10px; z-index: 2;
    color: #d1d4dc; font-weight: 600; font-size: 13px;
    background: rgba(19,23,34,0.7); padding: 2px 6px; border-radius: 3px; }
  .chart { position: absolute; inset: 0; }
  #readout { position: absolute; top: 8px; right: 14px; font: 12px ui-monospace,
    monospace; color: #d1d4dc; text-align: right; white-space: pre; }
</style>
<header><h1>__TITLE__</h1><div id="meta">__META__</div>
<div id="readout"></div></header>
<div id="panes"></div>
<script>
const PANES = __DATA__;
const root = document.getElementById('panes');
const charts = [];
for (const pane of PANES) {
  const box = document.createElement('div');
  box.className = 'pane';
  const label = document.createElement('div');
  label.className = 'label';
  label.textContent = pane.label + ' (' + pane.meta + ')';
  const el = document.createElement('div');
  el.className = 'chart';
  box.appendChild(label);
  box.appendChild(el);
  root.appendChild(box);
  const chart = LightweightCharts.createChart(el, {
    autoSize: true,
    layout: { background: { color: '#131722' }, textColor: '#d1d4dc' },
    grid: { vertLines: { color: '#1f2430' }, horzLines: { color: '#1f2430' } },
    rightPriceScale: { borderColor: '#2a2e39',
      scaleMargins: { top: 0.06, bottom: 0.28 } },
    timeScale: { borderColor: '#2a2e39', timeVisible: true,
      secondsVisible: __SECONDS__ },
    crosshair: { mode: LightweightCharts.CrosshairMode.Normal },
  });
  const candles = chart.addCandlestickSeries({
    upColor: '__UP__', downColor: '__DOWN__',
    borderUpColor: '__UP__', borderDownColor: '__DOWN__',
    wickUpColor: '__UP__', wickDownColor: '__DOWN__',
  });
  candles.setData(pane.candles);
  const vol = chart.addHistogramSeries({
    priceFormat: { type: 'volume' }, priceScaleId: 'vol', color: '__UP__',
  });
  chart.priceScale('vol').applyOptions({
    scaleMargins: { top: 0.78, bottom: 0 } });
  vol.setData(pane.volumes);
  charts.push(chart);
}
// Link the visible logical ranges. Every pane has bars on the same minute
// grid from the same anchor, so a logical range maps to the same minutes in
// each; syncing by logical index rather than by time keeps panes aligned even
// where one pane has an empty minute the other lacks.
let syncing = false;
for (const chart of charts) {
  chart.timeScale().subscribeVisibleLogicalRangeChange((range) => {
    if (syncing || !range) return;
    syncing = true;
    for (const other of charts) {
      if (other !== chart) other.timeScale().setVisibleLogicalRange(range);
    }
    syncing = false;
  });
}
for (const chart of charts) chart.timeScale().fitContent();

// Hover readout: the bar index under the crosshair, its minute in UTC and
// Chicago (daylight offset, matching the preset's permanent CDT clock), and
// every pane's range and volume at that bar. Panes share one grid, so one
// logical index names the same minute in all of them.
const readout = document.getElementById('readout');
const grid = PANES[0].candles.map((c) => c.time);
const pad = (n) => String(n).padStart(2, '0');
const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
function stamp(secs) {
  const d = new Date(secs * 1000);
  const hm = DAYS[d.getUTCDay()] + ' ' + pad(d.getUTCHours()) + ':' + pad(d.getUTCMinutes());
  return __SECONDS__ ? hm + ':' + pad(d.getUTCSeconds()) : hm;
}
function describe(index) {
  if (index < 0 || index >= grid.length) return '';
  const t = grid[index];
  let text = 'bar ' + index + '  ' + stamp(t) + ' UTC  ' + stamp(t - 5 * 3600) + ' Chicago';
  for (const pane of PANES) {
    const c = pane.candles[index];
    const v = pane.volumes[index];
    if (c.open === undefined) {
      text += '\\n' + pane.label + ': closed';
    } else {
      text += '\\n' + pane.label + ': H-L ' + (c.high - c.low).toFixed(2)
        + '  H ' + c.high + '  L ' + c.low + '  C ' + c.close + '  vol ' + v.value;
    }
  }
  return text;
}
for (const chart of charts) {
  chart.subscribeCrosshairMove((param) => {
    if (param.logical === undefined || param.logical === null) return;
    readout.textContent = describe(Math.round(param.logical));
  });
}
</script>
"""


def main(argv: list[str]) -> None:
    p = argparse.ArgumentParser(prog="compare.py")
    p.add_argument("--out", required=True, help="HTML to write")
    p.add_argument("--title", default="Tape comparison")
    p.add_argument(
        "--interval",
        type=int,
        default=60,
        help="bar interval in seconds; the shared grid steps by it",
    )
    p.add_argument(
        "panes",
        nargs="+",
        metavar="LABEL=CSV",
        help="a pane label and the bars CSV it shows",
    )
    args = p.parse_args(argv)

    loaded = []
    for spec in args.panes:
        label, _, path = spec.partition("=")
        if not path:
            sys.exit(f"compare: expected LABEL=CSV, got {spec!r}")
        kind, rows = read_rows(path)
        if kind != "bars":
            sys.exit(f"compare: {path} is a {kind} CSV; only bars stack")
        candles, volumes = bar_series(rows)
        # An empty bar is a closure minute the generator carried forward. It
        # is not a bar; it becomes a gap like the real feed's missing minute.
        traded = [
            (c, v) for c, v in zip(candles, volumes) if v["color"] != EMPTY
        ]
        loaded.append((label, traded))

    # The grid is every minute from the first traded one to the last, not
    # the union of traded minutes: a closure the real feed omits must
    # occupy its width on the page, or a reopen gap reads as a jump between
    # neighbouring bars and the hour that passed is invisible.
    first = min(traded[0][0]["time"] for _, traded in loaded if traded)
    last = max(traded[-1][0]["time"] for _, traded in loaded if traded)
    grid = list(range(first, last + args.interval, args.interval))
    payload = []
    for label, traded in loaded:
        by_time = {c["time"]: (c, v) for c, v in traded}
        candles = []
        volumes = []
        for t in grid:
            if t in by_time:
                c, v = by_time[t]
                candles.append(c)
                volumes.append(v)
            else:
                candles.append({"time": t})
                volumes.append({"time": t})
        week_range = max(c["high"] for c, _ in traded) - min(
            c["low"] for c, _ in traded
        )
        payload.append(
            {
                "label": label,
                "meta": f"{len(traded)} bars, week range {week_range:.0f}",
                "candles": candles,
                "volumes": volumes,
            }
        )

    meta = (
        "Time axis in UTC. Scroll or zoom any pane and the others follow; "
        "the same screen position is the same minute in every pane, and a "
        "closure is a gap in all of them. Each pane keeps its own price "
        "scale, so read the week range in the label before comparing bar "
        "heights across panes."
    )
    page = TEMPLATE
    for token, value in (
        ("__TITLE__", html.escape(args.title)),
        ("__META__", html.escape(meta)),
        ("__UP__", UP),
        ("__DOWN__", DOWN),
        ("__SECONDS__", "true" if args.interval < 60 else "false"),
    ):
        page = page.replace(token, value)
    page = page.replace("__DATA__", json.dumps(payload, separators=(",", ":")))
    out_dir = os.path.dirname(os.path.abspath(args.out)) or "."
    os.makedirs(out_dir, exist_ok=True)
    with open(args.out, "w") as f:
        f.write(page)
    print(
        f"compare: wrote {args.out} with {len(payload)} panes",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main(sys.argv[1:])
