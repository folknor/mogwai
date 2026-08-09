# Data purchases: the standing record

Consolidated 2026-08-09 from the working research report that grew here
between 2026-08-03 and 2026-08-06. The full 2,687-line history - the
question as originally framed, the CME bar-archive findings, the candidate
baskets, the two preregistered experiments, the spread experiment contract,
and the corrections log - lives in git history; this page carries what an
agent acting today needs. Any citation elsewhere of a section number this
page no longer carries resolves to git history.

Notes-class despite living at the root: transient, no truth guarantee,
nothing durable may cite it.

## Money

Owner policy, restated 2026-08-05: CREDIT-FIRST. The 125 dollars of
Databento credit is spent as optimally as possible first; personal cash
(roughly 100 more) is a CAN, argued case by case, put to the owner only on
joint reviewer agreement, never a committed path.

| | |
|---|---:|
| Databento credit | 125.00 |
| Wave 1, the paired NQ/MNQ test, job GLBX-20260805-JUBCRPRLG8 | 24.06 |
| Wave 2B July, the MNQ TBBO month, job GLBX-20260805-HAPEWPABKG | 73.41 |
| **Credit remaining (contingency, not re-buy capacity)** | **27.53** |

Both purchases are delivered, hash-verified, ledger-bound
(`analysis/databento-jobs.json`) and fully consumed by the protocol 10, 11
and 12a landings. NOTHING further is authorized. The named next marginal
dollar is a ~30 dollar ES/MES buy so the MES preset stops being inherited
faith - owner-gated, not urgent, recorded here since 2026-08-05.

## Standing verdicts that gate future buying

- **NQ is NOT an MNQ proxy.** The wave 1 pair test FAILED under its frozen
  preregistration (`notes/pair-test-preregistration.md`, still the live
  judge): mandatory P5 failed on `zero_change_frac`, the
  activity-tracking failure predicted in advance. Verdict artifact
  `analysis/databento-pair-verdict.json`, bound to job, hashes, prereg
  hash and harness commit; one run, final. Consequence: wave 2A (four NQ
  months, 71.79) is PERMANENTLY SKIPPED and structurally locked in the
  downloader's stage gate.
- **Volatility-stratified window selection is REJECTED.** The
  sampling-frame experiment (`notes/sampling-frame-preregistration.md`)
  failed its preregistered association test on 19 months of BTCUSDT;
  contiguous recent months replaced regime-selected windows. Scope
  caveat that keeps being missed: only the rv-rank association was
  tested; the five-feature farthest-point selection was never on trial.
- **`mnq06` (the June month, 70.11, personal cash) is priced but
  deliberately NOT buy-whitelisted.** Its case must come from a decision
  contract that does not exist; July's delivery must never become its
  unlock. Absence from the whitelist fails closed.
- **Wave 3 (MNQ MBO, 54.52) is ALWAYS LAST and spec-gated**: it needs an
  accepted implementation spec for where order-lifecycle evidence lands
  in the venue (book, queue, cancellation dynamics) BEFORE purchase, plus
  its own authorization. MBP-1 (44.64) is the fallback if order identity
  proves unneeded; MBP-10 rejected as dominated.
- **The 12a verdict implicates the generator, not the evidence base** -
  `no-family-eligible` with arrival composition the loudest finding - so
  protocol 12b requires NO new data. The blocking gap is generated-side.

## The tools (all Python, permanently out of the Rust rewrite's scope)

- `analysis/databento_price.py` - pricing, free `metadata.*` and
  `symbology.resolve` calls only, response-cached
  (`analysis/databento_cache.json`). Hard invariant, twice reviewed: no
  reachable path to `timeseries.get_range` or `batch.submit_job`; it
  cannot spend.
- `analysis/databento_download.py` - the purchase tool. Dry-run default;
  submission needs `--confirm` AND a cumulative `--max-dollars` cap;
  fresh re-price at submit refusing above-cap or >10 percent drift;
  durable pre-POST intent with lag-aware reconciliation (it caught a real
  504 and adopted the job instead of double-buying); buys whitelisted to
  staged plans with verdict-artifact stage gates that fail closed.
- `analysis/pair_harness.py` - selftest / preflight / report over
  delivered pair data; loads the frozen preregistration JSON; never
  writes the verdict artifact itself.
- Delivered data lives under `research/market-data/databento/`
  (gitignored) with per-directory manifests.

## Traps already paid for - do not re-derive

- The DBN side alphabet is B=buy, A=SELL, N=none
  (`research/dbn/rust/dbn/src/enums.rs`); reading B/S manufactures a
  FAIL.
- Binance monthly SPOT trades are SEVEN columns, headerless; the daily
  FUTURES archives are six columns WITH a header. Assuming either schema
  for the other mis-parses every row.
- The delivered CME csv echoes continuous labels (`NQ.v.0`) in the symbol
  column, so the minority-contract guard is blind there;
  `instrument_id` is the sharper witness.
- Submit format flags are DECIDED and frozen by the wave 1 delivery:
  `pretty_px = False`, `pretty_ts = False`, `map_symbols = True`. Format
  continuity with the only delivery ever consumed beats theoretical
  improvement.
- The SDK deprecates `metadata.get_cost`'s `mode` parameter; pricing and
  downloader still send it so their quotes stay comparable. It
  demonstrably works; migrating is a later coordinated change to both
  scripts with a baseline refresh.
- Kraken carries whole-second timestamps on every row, so it can never
  adjudicate sub-second structure; the cadence lineage is Binance.
- Databento date-only bounds are UTC and clip the CME 17:00 Central
  session boundary; bounds must be explicit UTC instants, DST included.
  Use `v.0` (volume-ranked continuous), never `c.0`.

## What the delivered evidence became

The July MNQ TBBO month fed three landed protocols - the protocol-10 fit,
the protocol-11 session refit, the protocol-12a measurement - whose
consolidated record is `notes/protocol-landings.md` and whose live
successor is the 12b spec. The instrument set today: MNQ and BTCUSDT carry
real fitted tapes; MES is a stated stopgap borrowing the MNQ fit (the
ES/MES purchase above is the recorded route to ending the borrow); the
ETHUSDT and SOLUSDT presets - BTCUSDT aliases with no evidence of their
own - were retired 2026-08-09 by owner ruling.

## Open items that survive the consolidation

- The ~30 dollar ES/MES purchase: named, owner-gated, unscheduled.
- Whether `zero_change_frac` should be DERIVED from the tick-to-price
  ratio rather than fitted - the strongest cross-corpus finding of the
  original report (it tracks activity, not the instrument; provable for
  free across hundreds of Kraken tick grids). Generator-design question,
  version-bumping.
- The dynamic (volatility-responsive) quoted-width and trade-displacement
  response: the per-instrument seams exist and are fitted static; making
  them respond to stress is a future spec with its own bump. Whether the
  delivered July month spans enough volatility range to fit it is
  MEASURED from the delivered data first; buying range is a later
  decision only if that measurement says it is missing.
- The MNQ chart PNG (`docs/example-generated-bars.png` counterpart):
  cosmetic, needs a browser.
