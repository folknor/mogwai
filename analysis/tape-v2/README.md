# tape-v2

The Python side of synthetic tape v2: corpus indexing, the fitting
experiments E0 to E6, and the validation battery, all run against the
Databento GLBX corpus. The programme is `notes/synthetic-tape.md`; what the
tree already binds is `notes/synthetic-tape-tree-context.md`.

The source lives here, in the mogwai tree, so it is versioned with the
presets it produces. It runs on the host that holds the corpus, `speilegg`,
where the data is under `/speilelg/databento` and the project is checked out
at `~/Claude/tape-v2` on the local SSD. The battery and the charts also run
on the tree's host, against artifacts pulled back from the run host.

## Running

Push the source, then run a subcommand remotely:

```sh
python3 analysis/tape-v2/sync.py
ssh speilegg uv --directory Claude/tape-v2 run tape-v2 index
```

`uv run` resolves the lockfile and installs the package on every call, so a
changed `pyproject.toml` needs no separate step. `uv.lock` is authoritative
and is committed; after an `add` on the run host, pull it back with
`python3 analysis/tape-v2/sync.py pull-lock`. Any single file comes back
with `sync.py pull REMOTE LOCAL`.

Outputs land in the project's `data/` on whichever host runs the command.
That folder is gitignored on both.

## Subcommands

- `index` scans every batch job directory once and writes
  `data/corpus-index.parquet`, one row per data file, so later experiments
  locate their inputs by schema and day instead of rescanning.
- `bars --parent P --first D --last D` writes one front-month week of real
  one-minute bars in the chart gate's CSV shape, for `plot_tape.py` and
  `compare.py`.
- `extract --parent P --first D --last D` caches the front month's
  one-minute bars for every day in the range as
  `data/bars-1m/P.parquet`, with the per-day contract choice beside it.
- `profile --parent P [--label L --csv ...]` builds the per-minute-of-
  session distribution of volume and range (the activity envelope), fits
  the intake knobs (phase multipliers, opening ramps, settlement spike),
  and writes matrix, profile and envelope under `data/profile/`. The
  default label `real` reads the extracted corpus bars; any other label
  names a candidate built from gen bars CSVs, one seed per file.
- `battery --parent P --candidate L` reports the candidate's containment
  inside the real p10 to p90 band per phase, with level ratios.
- `envelope-chart --parent P --candidate L ... --out page.html` renders
  the real band and median with candidate medians overlaid.
- `status-probe --parent P --day D` prints the exchange's session state
  transitions for the parent's outrights on one day.
- `envelope-toml --parent P --corpus TEXT --window TEXT` writes the
  preset's `[instrument.calendar.envelope]` block and its provenance
  entries from the real profile, to `data/profile/P-envelope.toml`;
  `splice_envelope.py PRESET BLOCK` puts it into a venue preset.
- `micro-extract --parent P --first D --last D` caches the front month's
  `tbbo` prints per day under `data/micro/tbbo/P/`, with the pre-trade
  touch beside each print and prices on the tick grid. A day is three
  seconds; the year is minutes with six workers.
- `micro-bars --parent P --first D --last D --interval S` writes real
  bars at any interval from the extracted prints in the chart CSV shape,
  so `compare.py --interval S` can stack a 15-second real week beside a
  generated one.
- `micro-stats --parent P [--label L --csv ... --tick T]` computes the
  sub-minute targets: the inter-parent gap law and the gap normalised by
  the minute's own rate, the within-window dispersion at 10 ms, 100 ms,
  1 s and 10 s, the sweep (children, levels, size, touch depletion), the
  aggressor sign memory and run lengths, the per-parent price change,
  the spread and the signed mid move after a parent. The real side runs
  per calendar month, a candidate per seed CSV (gen trades, with `--tick`
  and the preset's fixed clock), and a candidate run prints the real
  median with its month-to-month p10 to p90 beside the candidate median,
  flagged where the candidate sits outside the band.

## Scripts beside the package

- `compare.py` stacks bars CSVs on one page: every pane on one shared
  minute grid so the same screen position is the same minute, closures as
  gaps, the week range in each label because the price scales differ, and
  a hover readout with the bar index, both clocks and each pane's bar.
- `sketch.py` rasterises bars CSVs to a PNG, for an agent's eye.
- `bars_profile.py` and `bars_ratio.py` are quick hourly views of a bars
  CSV; the second compares absolute levels, which the first cannot.
- `bars_phases.py`, `bars_moves.py`, `bars_tail.py`, `bars_bigmoves.py`
  and `bars_volume_texture.py` are the eye's arithmetic on a week: silent
  runs and per-phase medians, phase travel and variance ratios, the
  largest minutes, the largest multi-minute moves with their start times,
  and the overnight volume quantiles and autocorrelation. `bars_at.py`
  prints the bars around a compare-page index.
- `residuals.py` divides a profile matrix by the real envelope and reports
  the residual's quantiles, log-sd and autocorrelation per phase, plus the
  session level and its day-to-day autocorrelation: the arrival-process
  targets. `price_targets.py` reports the price-side targets from bars,
  real or candidate: minute-return sd and kurtosis, variance ratios, the
  largest moves per session by horizon, range efficiency, gaps.
  `vr_long.py` and `acf_long.py` take the variance ratio and the pooled
  autocorrelation out to the session horizon.
- `proto_engine.py` is the one-second prototype of the activity cascade
  the Rust engine transcribes, with `proto_martingale_probe.py` and
  `proto_efficiency_probe.py` as its component-by-component probes.
- `proto_micro.py` is the prototype of the layer below the second: the
  branching placement under the cascade's rate, the fast texture and the
  order-splitting sign model, measured by `micro-stats`' own function and
  scored against the real medians as a mean absolute log miss; `--grid`
  scores a JSON list of placement configurations in one run, and
  `micro-grids/` holds the two grids the protocol 33 fit was chosen from.
- `proto_impact.py` fits the propagator, a parent's permanent and
  transient impact on the mid, on the splitting sign model against the
  real signed mid move at one, ten and a hundred parents; `--grid` scores
  the parameter grid the protocol 34 values came from.
- `open15.py` is the owner's-scale gate for the cash open: the first hour
  on 15-second bars, per session, bar range quantiles, travel, hour range
  and the variance ratios from 15 seconds to 15 minutes, real across the
  year (`--real`, on the run host) or a gen 15-second bars CSV (`--csv`).
- `sync.py` moves the project between the tree and the run host.
