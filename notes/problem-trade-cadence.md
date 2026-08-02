# PROBLEM: the tape is two orders of magnitude too slow, and "trades per second" is undefined

Expanded from what would otherwise be a `notes/todo.md` entry; it outgrew a
bullet. Everything downstream of arrival rate inherits the decisions here, so
this is the first of the arrival/profile/book documents to resolve.

## What the user wants

Looking at the generated tape the user said there are still way too few trades
in some places, and asked what an active crypto pair actually does per second
on a weekday - the answer being roughly 1-5 trades/sec on a mid-tier venue and
tens per second on Binance, against the 0.14/sec mogwai produces. They want the
default tape to look like a real active instrument rather than a thin one, they
consider the Kraken lineage incidental (it was an easy place to get a lot of
data, not a claim about which venue mogwai is), and they expect this to want
havoc knobs as well as an honest default, though which knobs and how was left
open.

## The observation

The default BTCUSDT tape runs at `mean_duration_s = 7.19`, which is 0.14
trades per second. Real Binance BTCUSDT in June 2026 ran at 49.6 raw trades
per second. That is a factor of 350.

This is not the drought defect, which was fixed on 2026-08-02 (`ca72e89`). That
one was about hour-scale SILENCE - a quiet stretch self-prolonging because the
ACD memory decayed per tick rather than in wall time. The dwell bound that
landed with it is holding: zero empty hours at both the test seed and the
production FNV seed, p999 gap 258 s against a 448 s bound. The tape no longer
goes dark. It is simply, uniformly, far too slow, and no committed gate says
otherwise because nothing sets a floor on density.

## Measured, from committed archives

Binance spot, June 2026, from 1s klines (`Number of trades` column) and
aggTrades. Archives are in `research/market-data/`, which is gitignored;
`analysis/probe_binance_aggtrades.py` computes the trade-level statistics.

| | BTC | ETH | SOL |
|---|---|---|---|
| raw trades/sec, mean | 49.6 | 46.9 | 12.5 |
| raw trades/sec, median | 4 | 3 | 1 |
| raw trades/sec, p95 | 257 | 233 | 57 |
| seconds with zero trades | 13.4% | 26.2% | 38.9% |
| mean trade size, native | 0.00492 BTC | 0.0890 ETH | 2.746 SOL |
| mean trade size, NOTIONAL | $311 | $151 | $191 |
| notional/sec | $15,426 | $7,084 | $2,393 |
| taker-buy share of volume | 0.483 | 0.495 | 0.496 |

Cross-validated: TradingView's 15s export of the same pair over a different
window gives 0.176 BTC/sec against the klines' 0.244 BTC/sec for June - same
order, independent source. The aggTrades and kline readings agree to three
digits on the aggregation factor (3.77) and on taker-buy share.

## The data, and how to reproduce every number in this document

**Where it lives.** `research/market-data/`. `research` is gitignored, so none
of it is committed and a fresh clone has none of it. Roughly 1.5 GB:

- `BTCUSDT-1s-{2026-04,05,06}.zip`, plus `ETHUSDT-` and `SOLUSDT-` for the same
  three months - Binance spot 1-second klines, monthly archives.
- `BTCUSDT-1s-2026-07-*.zip` - July dailies, because Binance publishes the
  monthly archive with a lag and July was not up yet. These overlap the
  TradingView export window, which is how the cross-validation above was done.
- `{BTCUSDT,ETHUSDT,SOLUSDT}-aggTrades-2026-06.zip` - trade-level, June only.
  BTC 498 MB, ETH 466 MB, SOL 91 MB.
- `BINANCE_BTCUSDT, 15S_*.csv` and `CME_MINI_MNQ1!, 15S_*.csv` - hand-exported
  from TradingView, 15-second OHLCV. The CME file is the only non-crypto data
  currently held and the only source for a futures session envelope.

**How to get more.** `research/binance-public-data/` is a vendored checkout of
Binance's own downloader. Its scripts need `pandas`, which is not installed
here, so the archives above were fetched straight off the published URLs
instead - the layout is stable and needs no dependency:

    https://data.binance.vision/data/spot/monthly/klines/<SYM>/1s/<SYM>-1s-<YYYY-MM>.zip
    https://data.binance.vision/data/spot/daily/klines/<SYM>/1s/<SYM>-1s-<YYYY-MM-DD>.zip
    https://data.binance.vision/data/spot/monthly/aggTrades/<SYM>/<SYM>-aggTrades-<YYYY-MM>.zip

That checkout also carries `download-trade.py`, which fetches RAW trades rather
than aggregated ones. Nothing here has used it yet, and it is the measurement
this document's clustering question actually needs - see below.

**The probes.** Both are committed, streaming, and O(1) in memory:

    python3 analysis/probe_binance_klines.py research/market-data/BTCUSDT-1s-2026-06.zip
    python3 analysis/probe_binance_aggtrades.py research/market-data/BTCUSDT-aggTrades-2026-06.zip

The kline probe produces the rate and size table above. The aggTrades probe
produces the duration statistics, and prints them twice: once over raw
aggTrades and once collapsed to distinct timestamps, which is the correction
the next section explains. It also prints the sweep-width distribution, which
is a by-product of the collapse and is new structure mogwai does not model.

Two Kraken-side helpers already exist from earlier work and are worth knowing
about here: `analysis/decode_dwell_bins.py` reads the committed
`analysis/char_*.json` histograms without needing the corpus disk at all, and
`analysis/probe_warmup_window.py` counts trades per simulated hour off a
running server's `/trades`. The Kraken corpus itself is at `/home/folk/Kraken`
(`MOGWAI_DATA_DIR`), and `analysis/run_corpus.py` plus
`analysis/build_fingerprint.py` are what regenerate the committed fingerprint
from it.

**A caveat that applies to every duration number below.** aggTrades timestamps
are MICROSECONDS - 16 digits, Binance moved off milliseconds - and the first
pass of this analysis divided by 1,000, producing a 30,000-day span for a
one-month file. Anything quoting an inter-trade duration from before that fix
is wrong by 1000x.

## The definitional problem, which must be settled first

"Trades per second" has three values on the same instrument in the same month,
differing by 8.5x on BTC:

| definition | BTC | what it is |
|---|---|---|
| raw trades | 49.6/sec | every fill, as counted by the kline `Number of trades` |
| aggTrades | 13.2/sec | fills merged by price and side within one taker order |
| match events | 5.8/sec | one taker order hitting the book, however many makers |

The collapse from aggTrades to match events is not a modelling choice - it is
recovering what the exchange actually did. Binance stamps every fill of one
match event with a single `transact_time`, so 55.5% of consecutive BTC
aggTrades share a timestamp. At 13 trades/sec, microsecond collisions cannot
happen by chance; they are the signature of a taker sweeping several makers.

A `TradeTick` on this venue is one print at one price with one size. Which of
the three it corresponds to is not obvious and IS the cadence target:

- **Match event** is what the exchange did, and mogwai has no book, so one
  synthetic trade being one taker order is arguably the honest analogue.
- **aggTrade** is what a real exchange WebSocket feed publishes, so it is what
  a consumer's tape would actually show. If mogwai's job is to look like a feed
  a client consumes, this is the target.
- **Raw trade** is what the kline count reports and is the number a naive
  "trades per second" comparison reaches for. It is the least defensible as a
  tape, because no venue publishes fills individually.

Nothing downstream can be sized until this is picked.

## Sizing is per-notional, not per-instrument

Mean trade size differs by three orders of magnitude in native units across
BTC, ETH and SOL, and by less than 2x in notional: $311, $151, $191. So the
current `typical_size` default (0.1, operator-set, per instrument, native
units) is the wrong shape of knob. A notional target divided by the
instrument's price gives a new instrument a defensible size from two numbers
already known, with no fit.

Consequence: mogwai's current tape is wrong in BOTH factors and they partly
cancel. Trades are ~20x too large and ~350x too rare, so volume is only ~17x
low. Anyone checking volume alone would under-read the defect.

## Why the dispersion band breaks on any rescale

`analysis/characterize.py` computes `dispersion_index` as `var / mean`, which
carries units of seconds. It is not scale-free: halve the cadence and the index
halves, whatever the shape does. The committed band - re-derived era-windowed
on 2026-08-02 to `[36.3 .. 1627.9]` - is stated in those units.

So a cadence change will fail the realism gate mechanically, for reasons that
have nothing to do with the arrival shape. Measured, same data, both forms:

| | Kraken XBTUSD (era-windowed) | Binance BTC (match events) | Binance SOL |
|---|---|---|---|
| mean gap | 2.871 s | 0.171 s | 0.516 s |
| `var / mean` (committed units) | 36.28 | 0.79 | 1.85 |
| `var / mean^2` (dimensionless) | 12.64 | 4.62 | 3.57 |

The 41x difference between Kraken and Binance in the committed units is
ENTIRELY cadence. In dimensionless terms Binance's two instruments sit within
30% of each other. Kraken at 12.64 remains an outlier and has not been checked
under the same timestamp-collapse, which is a measurement this document's
resolution needs.

## What must be decided

1. **Which definition of a trade the tape emits.** Match event, aggTrade, or
   raw fill. Everything else is sized off this.
2. **The target rate**, given that definition. Mean and median differ by 12-15x
   on every instrument measured, so "the rate" is itself ambiguous: targeting
   the mean builds a tape that is busy almost always, which is not what any of
   the three instruments look like.
3. **Whether the dispersion band is restated dimensionlessly** or rescaled with
   the cadence. Restating is the honest fix; rescaling preserves the committed
   numbers at the cost of keeping a scale-dependent gate.
4. **Whether trade size becomes notional-derived.**

## What this document does not decide

Per-instrument profiles, which are the subject of the sibling document - this
one settles what a trade is and how fast they arrive for ONE instrument.
Clustering constants and whether they are shared or per-instrument, likewise.
The sweep structure the collapse exposed (76.5% of BTC match events are one
print, tail to 2,213) is real and unmodelled, but it needs levels to land in,
so it belongs to the book document. No change to the drought dwell bound, which
is orthogonal and holding.

## Known cost, not yet priced

A 30x denser tape is 30x the ticks per unit of sim time. Everything sized
against the current cadence moves with it, and one of them is a correctness
risk rather than a cost: `SWEEP_DRAIN_BUDGET` (20,000) bounds one sweeper pass,
and a pass that truncates returns `complete: false` and stalls the scan
frontier. `MAX_HISTORY_SEEK_TICKS`, `CHECKPOINT_K` and the 24 h backfill
horizon are cost. The golden fill distribution and every realism anchor
re-bless. `reference/performance.md`'s figures were measured on the sparse tape
and would need re-running - the benches for that landed on 2026-08-02
(`62f936f`), so the instrument exists.
