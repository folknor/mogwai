"""Render activity envelopes as one HTML page: real band and median per
minute of session, with any number of candidate medians overlaid.

Two linked charts, volume and range, over the 1,380 minutes of a session.
The x axis is a clock: minute of session is mapped onto a Monday in
August so the labels read as a UTC time, with the session opening at
22:00 UTC (17:00 Chicago daylight time). Stdlib rendering, lightweight
charts from the CDN like the chart gate.
"""

from __future__ import annotations

import html
import json
from pathlib import Path

import polars as pl

# Monday 2026-08-17 22:00 UTC, the 17:00 CDT open of a Tuesday session.
AXIS_ORIGIN_S = 1786917600 + 86400

REAL_MEDIAN = "#d1d4dc"
REAL_BAND = "#4b5563"
CANDIDATE_COLORS = ["#f59e0b", "#26a69a", "#ef5350", "#60a5fa"]

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
  #legend { margin-top: 3px; font-size: 12px; }
  #legend span { margin-right: 14px; }
  #panes { position: absolute; top: 66px; left: 0; right: 0; bottom: 0;
    display: flex; flex-direction: column; }
  .pane { flex: 1 1 0; position: relative; border-top: 1px solid #2a2e39; }
  .label { position: absolute; top: 6px; left: 10px; z-index: 2;
    font-weight: 600; background: rgba(19,23,34,0.7); padding: 2px 6px; }
  .chart { position: absolute; inset: 0; }
</style>
<header><h1>__TITLE__</h1><div id="meta">__META__</div>
<div id="legend">__LEGEND__</div></header>
<div id="panes"></div>
<script>
const DATA = __DATA__;
const root = document.getElementById('panes');
const charts = [];
for (const pane of DATA.panes) {
  const box = document.createElement('div');
  box.className = 'pane';
  const label = document.createElement('div');
  label.className = 'label';
  label.textContent = pane.label;
  const el = document.createElement('div');
  el.className = 'chart';
  box.appendChild(label);
  box.appendChild(el);
  root.appendChild(box);
  const chart = LightweightCharts.createChart(el, {
    autoSize: true,
    layout: { background: { color: '#131722' }, textColor: '#d1d4dc' },
    grid: { vertLines: { color: '#1f2430' }, horzLines: { color: '#1f2430' } },
    rightPriceScale: { borderColor: '#2a2e39', mode: pane.log ? 1 : 0 },
    timeScale: { borderColor: '#2a2e39', timeVisible: true,
      secondsVisible: false },
    crosshair: { mode: LightweightCharts.CrosshairMode.Normal },
  });
  for (const s of pane.series) {
    const line = chart.addLineSeries({ color: s.color, lineWidth: s.width,
      priceLineVisible: false, lastValueVisible: false });
    line.setData(s.points);
  }
  charts.push(chart);
}
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
</script>
"""


def _points(profile: pl.DataFrame, column: str) -> list[dict]:
    frame = profile.sort("session_minute")
    return [
        {"time": AXIS_ORIGIN_S + 60 * int(m), "value": float(v)}
        for m, v in zip(
            frame["session_minute"].to_list(),
            frame[column].to_list(),
            strict=True,
        )
    ]


def render(
    real: pl.DataFrame,
    candidates: list[tuple[str, pl.DataFrame]],
    out: Path,
    title: str,
) -> None:
    panes = []
    legend = [f'<span style="color:{REAL_MEDIAN}">real median</span>',
              f'<span style="color:{REAL_BAND}">real p10 and p90</span>']
    panes_spec = (
        ("volume_norm", True, "shape: volume / session mean minute"),
        ("range_norm", False, "shape: range / session median minute"),
        ("volume", True, "level: raw volume per minute"),
        ("range", False, "level: raw range per minute"),
    )
    for quantity, log, caption in panes_spec:
        series = [
            {
                "color": REAL_BAND,
                "width": 1,
                "points": _points(real, f"{quantity}_p10"),
            },
            {
                "color": REAL_BAND,
                "width": 1,
                "points": _points(real, f"{quantity}_p90"),
            },
            {
                "color": REAL_MEDIAN,
                "width": 2,
                "points": _points(real, f"{quantity}_p50"),
            },
        ]
        for i, (label, frame) in enumerate(candidates):
            color = CANDIDATE_COLORS[i % len(CANDIDATE_COLORS)]
            series.append(
                {
                    "color": color,
                    "width": 2,
                    "points": _points(frame, f"{quantity}_p50"),
                }
            )
            if quantity == "volume_norm":
                legend.append(f'<span style="color:{color}">{label}</span>')
        panes.append(
            {
                "label": caption + (" (log scale)" if log else ""),
                "log": log,
                "series": series,
            }
        )
    meta = (
        "x axis: minute of session shown as a UTC clock with the 17:00 "
        "Chicago open at 22:00 UTC (daylight time). Cash open 08:30 Chicago "
        "is 13:30 UTC; settlement 15:00 Chicago is 20:00 UTC."
    )
    page = TEMPLATE
    for token, value in (
        ("__TITLE__", html.escape(title)),
        ("__META__", html.escape(meta)),
        ("__LEGEND__", " ".join(legend)),
    ):
        page = page.replace(token, value)
    page = page.replace(
        "__DATA__", json.dumps({"panes": panes}, separators=(",", ":"))
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(page)
    print(f"wrote {out}")
