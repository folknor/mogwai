# Python script triage, for the measurement-machinery Rust rewrite

Scope question per script: KEEP (stays Python), ABSORB (belongs in the new
Rust crate), DEAD (one-shot, findings already frozen in a committed artifact,
deletable). 49 scripts on disk (44 `analysis/`, 5 `scripts/`); 31 named by
DATA-PURCHASE-REPORT.md/notes, 18 unnamed, 3 claimed-ghosts checked.

## The named 31 (+ __init__.py)

- **KEEP `databento_price.py`** - live pricing tool, hits only free
  `metadata.*`/`symbology.resolve` endpoints, hard-invariant-verified it can't
  spend. Explicitly audited/ledger-bound, stays Python per your framing.
- **KEEP `databento_download.py`** - the doubly-gated purchase downloader,
  imports scopes/plans from `databento_price.py`. Same bucket, live money tool.
- **KEEP `pair_harness.py`** - wave-1 NQ/MNQ acceptance harness, reads the
  frozen `pair-test-preregistration.json`; still the live judge for any future
  delivered pair data (`databento-pair-verdict.json` cites it). Imports
  `probe_binance_aggtrades.AutoCorr`/`probe_binance_trades.EventStats`
  unchanged - so those two stay KEEP too, not DEAD, despite being "probes".
- **KEEP `probe_binance_aggtrades.py`** - not a one-shot: `AutoCorr` is
  imported live by `build_cadence.py`, `build_targets.py`,
  `conformance_f3_f6.py`, `pair_harness.py`, `test_characterize.py`. Load-bearing library, not exploratory chaff despite its own docstring calling itself "exploratory."
- **KEEP `probe_binance_trades.py`** - same story, `EventStats` imported by
  `build_cadence.py`, `build_targets.py`, `resolution_fidelity.py`,
  `test_characterize.py`.
- **KEEP `binance_archive.py`** - live archive-acquisition survey tool
  (index/transition/verify/fetch), gates the still-open sampling-frame
  purchase question, network-facing, not a fit-era one-shot.
- **KEEP `preflight.py`** - the sampling-frame corpus preflight
  (validate/run/report/migrate), imported by `build_bars.py` and
  `build_targets.py`. Its `_byte_lines` is imported by `probe_binance_trades.py`
  too. Core library.
- **KEEP `asof_join.py`** - the join-semantics contract + fixture, referenced
  by `binance_spread.py` as the tested-equivalent reference implementation.
  Still the semantic source of truth if a real Databento quote/trade join is
  ever needed again.
- **KEEP `roll_estimator.py`** - shared fixture (`spread_conformance.json`)
  runs on BOTH sides (Python here, Rust in `mogwai-data`); the doc is explicit
  this stays split so neither language can quietly redefine the estimator.
  This is the intentional dual-implementation exception to "absorb the method."
- **DEAD `binance_spread.py`** - one-shot spread smoke tool over a specific
  delivered Binance pair; nothing currently cites its output as a
  frozen/committed artifact and no doc lists it as a live gate. Kept alive
  only by `asof_join.py` import, not the reverse. Superseded by the pair
  harness for anything MNQ-relevant.

- **ABSORB `mnq_fit.py`** (~9.5k lines) - the live measurement/fit harness
  itself: this IS "the measurement machinery" named in the rewrite mandate.
  Already has a partial Rust twin for the generated side
  (`crates/mogwai-server/src/measure12a.rs`, consumer-only). Per the report's
  own estimate: ~10% MNQ-specific config, ~15% CME/Databento-specific parsing,
  ~75% instrument-agnostic method - the 75% is exactly the ABSORB core. The
  selftest third is flagged in the report as entangled across layers and needs
  re-fixturing at the abstraction seam; do not treat it as free to port
  mechanically.
- **ABSORB `fit_session_profile.py`** - the SessionProfile fitter (NQ archive
  -> `presets/mnq.toml` scalars) and its preflight gate. Same method-shaped
  problem as `mnq_fit.py`'s session work; a separate script today only because
  it predates the harness's own session logic. Natural absorb target,
  probably folds into the same crate as a subcommand.
- **ABSORB `tick_composition_ratios.py`** - reads generator output and prints
  decomposition/stratified matrices (`--mode projection`/`independent`); its
  Rust sibling already exists (`brokkr run mogwai -- tick-composition`,
  `stratified_roll_matches_the_shared` test). The Python half is the
  cross-language conformance leg the doc says must stay split for
  `roll_estimator.py`'s reasons - but tick-composition's own doc language
  ("first two fast, third `#[ignore]`d as a report") suggests this one is
  closer to a report generator than a semantics-defining twin. Judgment call:
  keep it split like roll_estimator, OR fold the Python read-and-report side
  into the new CLI once it can read generator CSV output natively. Either way
  it belongs in scope discussion, not DEAD.
- **KEEP `plot_tape.py`** - HTML chart renderer over `mogwai gen` CSV output,
  a dev/debug visualization tool, not measurement. No reason to port a
  TradingView-JS-embedding script to Rust.
- **DEAD `build_bars.py`** - one-shot bar-construction + vendor-kline
  crosscheck for the sampling-frame experiment. Its `crosscheck` gate result
  and `build` output feed `select_windows.py`, whose own findings are frozen
  in `targets-frozen.json`. Nothing currently reruns this; imports
  `preflight.py` (KEEP) so isn't fully inert, but its own purpose is spent.
- **ABSORB `build_fingerprint.py`** - synthesizes the committed
  `fingerprint.json` contract from `char_*.json` per-pair characterizations.
  This is fingerprint generation, i.e. exactly measurement-machinery output
  the Rust crate should be able to reproduce/regenerate, not a frozen
  historical artifact - `fingerprint.json` is a live input to the generator
  fit, re-run whenever the corpus or method changes.
- **ABSORB `characterize.py`** - the phase-0 streaming stylized-fact
  characterizer, the estimand definitions the fingerprint and later fit code
  both build on (`LVL_BINS`, `histogram_quantile`, `lvl_bin`, `AutoCorr`-style
  ring-buffer ACF). Core, reusable, instrument-agnostic method - textbook
  absorb candidate, and it's the file most other scripts import from
  (`test_characterize.py`, `probe_kraken_durations.py`,
  `probe_timestamp_precision.py`, `run_corpus.py`, `build_targets.py`).
- **KEEP `test_characterize.py`** (as Python, until the port lands) - unit
  tests over `characterize.py`/`build_fingerprint.py`/`build_cadence.py`
  boundary-sensitive logic. Once those modules are absorbed, this either
  disappears (superseded by Rust unit tests in the crate) or narrows to
  whatever stays Python. Not itself an absorb target - it's a test file, not
  method.
- **DEAD `recon.py`** - phase-0 reconnaissance over the Kraken dump, output is
  `analysis/recon.json` (NOT committed - checked, absent from `git ls-files`).
  One-shot manifest builder over a corpus that lives outside the repo on a
  separate drive (`/home/folk/Kraken`); nobody downstream imports it. Fully
  spent.
- **DEAD `run_corpus.py`** - drives `characterize.py` across representative
  Kraken pairs, writes `char_<PAIR>.json` - which is **gitignored**
  (`/analysis/char_*.json` in `.gitignore`), not committed. Its own output is
  not the frozen artifact; `fingerprint.json` (the downstream synthesis) is
  what's committed. One-shot corpus driver, spent once the fingerprint fit.
- **DEAD `conformance_f3_f6.py`** - explicitly self-described as NOT
  preregistered and NOT blind, a post-hoc conformance check whose own
  docstring says "must never be described as" a preregistered claim. Frozen
  result: `conformance-f3-f6-expected.json` (committed). Superseded doc
  artifact, one-shot, imports the now-real `build_targets.py`.
- **ABSORB `select_windows.py`** - the sampling-frame window-selection
  stratifier (features/select/drift/plan). Method (stratify on cheap-bar
  features to pick tick-purchase windows) is instrument-agnostic and reusable
  for any future purchase decision, not spent by protocol landing the way a
  pure one-shot fit is. Its frozen decision lives in `targets-frozen.json`,
  but the STRATIFICATION METHOD itself is durable machinery worth keeping
  live, arguably in the new crate as a "which window to buy next" tool.
- **DEAD `inspect_archive.py`** - streaming Binance ZIP contract inspector,
  read-only fact-reporter used during corpus survey. Nothing currently cites
  a committed inspect-archive artifact; superseded by `preflight.py`'s
  integrated pass for anything that matters now. Reusable in principle but
  nothing currently uses it - park as DEAD, resurrect if a new archive shows
  up.
- **DEAD `inspect_cme_bars.py`** - "are these usable as a sampling frame"
  first-pass over the CME vendor bar zips; answered, folded into
  `select_windows.py`/`build_bars.py`, no committed artifact of its own
  (prints only). Spent.
- **DEAD `probe_timestamp_precision.py`** - answered the Kraken
  whole-second-timestamp question once (documented inline: "61% of
  consecutive trades record a gap of exactly zero"), which is now a frozen
  fact cited by `probe_kraken_durations.py`'s own docstring and by
  `resolution_fidelity.py`'s design. No artifact, but the finding is
  transitively frozen in the design of the scripts that came after it. One-shot,
  spent.
- **DEAD `stratum_occupancy.py`** - power-check calculator for held-out
  stratum cell counts, pure math no data, answered before any archive was
  downloaded. Preregistration-era one-shot, no artifact, nothing imports it.
- **DEAD `resolution_fidelity.py`** - the resolution-fidelity gate, output
  `analysis/resolution-fidelity-2026-06.json` is COMMITTED. Textbook frozen
  one-shot: verdict banked, machinery spent. Imports `probe_binance_trades`
  (KEEP) but is itself terminal.
- **KEEP `__init__.py`** - not a script, just makes `analysis/` importable for
  `python3 -m unittest discover`. Stays until/unless the whole tree is
  retired.
- **KEEP `scripts/smoke.py`** - THE launcher-contract reference
  implementation/smoke test (`docs/cli.md`), actively exercised, not
  measurement machinery at all. Out of this triage's scope by subject but
  obviously not going anywhere.

### The three claimed-ghosts

- **`side_predicate_scan.py`** - confirmed ABSENT from disk. Docstring debt:
  `notes/sampling-frame-preregistration.md:600` still cites it ("retained
  until this record landed") - dangling reference, harmless (past tense,
  correctly implies deletion), but worth a note-fix if that doc is ever
  touched again.
- **`fetch_binance_months.py`** - confirmed ABSENT from disk.
  `notes/sampling-frame-preregistration.md:901` still names it as "a
  fail-closed downloader for spot" - same dangling-reference situation.
- **`build_targets.py`** - **NOT a ghost. It exists on disk, 356 lines,
  committed, and is actively imported by `conformance_f3_f6.py`.** The task
  brief's claim that this one is gone is wrong; flagging as a correction
  rather than filing it as DEAD-because-absent. Verdict on the file itself:
  **DEAD** - its `build`/`freeze` output is frozen in the committed
  `targets-frozen.json` and `association-result.json`, its `equivalence` gate
  is a one-shot self-check against `cadence.json`. Spent as a driver, though
  `compute_targets` is imported live by `conformance_f3_f6.py` (itself DEAD)
  so nothing outside this now-DEAD cluster depends on it.

## The 18 unnamed scripts

- **DEAD `bench_rowparse.py`** - pure micro-benchmark, settled a hot-loop
  parsing question for `preflight.py` empirically. No output artifact, answer
  presumably already baked into `preflight.py`'s chosen parse strategy. Spent.
- **ABSORB `build_cadence.py`** - builds the committed `cadence.json` from
  raw-trade archives via `probe_binance_trades.probe`; PAIRS/WIDEN/`band()`
  are re-mirrored into `fingerprint.json`. This is measurement machinery
  (re-derives a fitted input whenever raw archives change), not a frozen
  one-shot - the doc explicitly distinguishes it from `rebuild_cadence_targets.py`
  ("run this instead whenever the MEASUREMENT changed"). Should live beside
  `characterize.py`/`build_fingerprint.py` in the new crate.
- **DEAD `check_additive.py`** - a git-diff guard restricted to
  fingerprint-regeneration additive-only changes. Useful CI-adjacent
  discipline but narrow, one constant-file-shaped guard; low value to port,
  low cost to keep as-is if kept at all. Leaning DEAD (not method, a linter).
- **ABSORB `check_cadence_feasible.py`** - reads `cadence.json`, issues the L0
  structural-proceed verdict via `next_count` simulation. This is a gate the
  fit pipeline runs, same status as `build_cadence.py` - live measurement
  logic, not frozen.
- **DEAD `decode_dwell_bins.py`** - review-validation helper, prints top
  duration bins from `char_*.json`. **Lateral finding: its docstring claims
  this lets reviewers check dwell claims "against committed data without the
  corpus disk" - false. `char_*.json` is gitignored
  (`.gitignore:15: /analysis/char_*.json`), confirmed absent from
  `git ls-files`.** Stale/wrong claim in the docstring; the committed
  artifact is `fingerprint.json`, not the per-pair char files. One-shot
  review helper regardless, DEAD.
- **DEAD `find_fixtures.py`** - one-shot search over 5,040 permutations to
  derive `spearman_association.py`'s hardcoded fixture constants "when a
  fixture constant needs to change; it is not part of any gate." Exactly the
  kind of derivation tool that's spent once the constant is committed.
- **KEEP `probe_binance_aggtrades.py`** - see above, promoted from probe to
  library by real imports.
- **DEAD `probe_binance_klines.py`** - exploratory probe of Binance 1s kline
  archives for arrival rate/trade size; no importer, no committed artifact.
  One-shot venue-comparison exploration.
- **DEAD `probe_kraken_durations.py`** - one-shot comparison of Kraken
  duration statistics against the Binance aggTrades finding, explicitly
  framed as checking whether Kraken "carries its own timestamp collisions."
  Answered, feeds the "committed band was re-derived against on 2026-08-02"
  fact, no ongoing role.
- **DEAD `probe_smoke.py`** - smoke test for `probe_binance_aggtrades.probe`
  and `probe_binance_trades.probe`'s rewritten `_byte_lines` parse path. Test
  of a specific rewrite; keep-worthy only as long as one wants a
  quick regression check on those two KEEP modules, but nothing currently
  runs it as a gate. Lean DEAD, low confidence - the two probes it tests are
  themselves KEEP/live, so this could arguably become a real unit test
  instead of being deleted. Flag for owner call rather than hard DEAD.
- **DEAD `rebuild_cadence_targets.py`** - cheap re-mirror path for when only
  the BAND RULE changed (not the measurement); reads `cadence.json`, rewrites
  `fingerprint.json`'s cadence block. Narrow maintenance utility over already-KEEP
  logic (`build_cadence.band`). Could be a CLI flag on the absorbed
  `build_cadence` rather than its own script - fold-in candidate more than a
  hard DEAD, but not separately worth porting as its own unit.
- **DEAD `relax_mean_calibration.py`** - adjudication sim for one specific
  wall-time-relaxation calibration decision (D2/Jensen drift), answered,
  numbers presumably baked into whatever the current ACD recursion constant
  is. Pure stdlib sim, no artifact, no importer.
- **DEAD `session_profile.py`** - text-bar renderer of `char_<PAIR>.json`
  seasonality ("a look, not a deliverable" per its own docstring). Dev
  convenience over gitignored cache input; nothing depends on it.
- **DEAD `spearman_association.py`** - the sampling-frame acceptance rule
  (selftest + report modes). **BUT it's imported by `find_fixtures.py`
  (also DEAD) and driven by `run_association.py` (also DEAD).** The rule's
  verdict is frozen in the committed `association-result.json`. Whole
  cluster is spent together; the acceptance-rule METHOD (Spearman rank
  association with permutation p-values) is generic enough it could be
  absorbed if a future sampling-frame decision is likely, but nothing on
  record says one is coming. Lean DEAD, not ABSORB, given no live consumer.
- **DEAD `run_association.py`** - "the deciding run," writes-once, refuses to
  overwrite an existing result. Result is `association-result.json`
  (committed). Definitionally one-shot by its own design ("a deciding file is
  written once").
- **DEAD `scripts/probe_arm_eviction.py`** - one-off correctness probe for
  the armed-divergence eviction-ack behavior, boots its own venue. This is
  engine-behavior verification, not measurement machinery, and out of this
  triage's real scope; noting DEAD-or-keep-as-regression-probe is an
  engine-team call, not a measurement-rewrite call. Leaving as is (out of
  scope).
- **DEAD `scripts/probe_blocked_sigterm.py`** - same bucket, PDEATHSIG/SIGTERM
  masking lifecycle probe. Out of measurement scope; not touched by this
  triage's verdict logic beyond noting it exists and isn't measurement.
- **DEAD `scripts/probe_orphan_guard.py`** - same bucket, orphaned-venue
  boot-path probe. Out of measurement scope.

Note on the three `scripts/probe_*` lifecycle probes: they're correctness
regression tools for the launcher/venue lifecycle, not measurement/fit
machinery, so "DEAD" above means "outside this triage's ABSORB candidate set,"
not "worthless" - do not delete them as part of a measurement-crate cleanup;
that's a separate decision for whoever owns lifecycle testing.

## What the ABSORB set implies for crate scope and name

ABSORB set: `mnq_fit.py`, `fit_session_profile.py`, `build_fingerprint.py`,
`characterize.py`, `build_cadence.py`, `check_cadence_feasible.py`,
`select_windows.py`, and (contested) `tick_composition_ratios.py`.

That's not "measurement only" - it's measurement AND fit AND fingerprint
synthesis AND window selection, i.e. the full offline-analysis-to-generator
pipeline minus the money tools and minus the two estimators (`roll_estimator.py`,
`asof_join.py`) that the report deliberately keeps split for cross-language
conformance reasons. A name like `mogwai-measure` undersells the scope (fit
and fingerprint synthesis aren't measurement, they're calibration/synthesis
downstream of it); `mogwai-fit` undersells it the other way (characterize.py
and build_cadence.py are pure measurement, no fitting). Given the actual
absorbed surface - characterize -> fit -> fingerprint/session-profile -> window
selection, all instrument-agnostic method with its own CLI - the crate is
closer to a tape-evidence toolbox than a single-purpose tool. Suggest
something like `mogwai-evidence` or `mogwai-corpus` over `mogwai-measure`;
whatever name is picked should not imply "just protocol-12a measurement,"
since that's ~10% of the file by the report's own line count and the other
ABSORB candidates are siblings, not subordinates.

Two structural constraints from the report worth restating here because they
bound the crate's shape, not just its name:
- Code moved beside the generator path loses the "analysis code needs no
  version bump" shield of 12a spec 2.3. A separate crate (not a module inside
  `mogwai-server`/`mogwai-data`) keeps `TAPE_PROTOCOL_VERSION` scoped, same
  precedent as `measure12a.rs` being consumer-only inside `mogwai-server`
  rather than defining anything `mogwai-data` exports.
- The selftest third of `mnq_fit.py` is entangled across the MNQ-specific /
  CME-specific / instrument-agnostic seams and needs re-fixturing per seam,
  not a mechanical port. Budget for that separately from the "75% is generic
  method" headline number.

## Ghost references worth fixing

- `notes/sampling-frame-preregistration.md:600` cites `analysis/side_predicate_scan.py`
  (absent) - past-tense phrasing already softens this, low priority.
- `notes/sampling-frame-preregistration.md:901` cites `analysis/fetch_binance_months.py`
  (absent) - same, low priority.
- The task brief itself is wrong that `build_targets.py` is a ghost - it's
  present, committed, 356 lines, live-imported by `conformance_f3_f6.py`.
  Whatever source fed that claim into the brief should be corrected too.

## Lateral findings

1. **`decode_dwell_bins.py`'s docstring is factually wrong**: claims dwell
   claims can be checked "against committed data" via `char_*.json`, but that
   glob is gitignored (`.gitignore:15`) and confirmed absent from
   `git ls-files`. Only the derived `fingerprint.json` is committed, not the
   per-pair intermediate. Low-stakes (dev-convenience script) but a false
   claim in a docstring that explicitly markets itself as reviewer tooling.
2. **`databento_cache.json` is committed** despite both the report (section
   13) and `databento_price.py`'s own docstring calling it "a regenerable
   cache... not source." Consistent with wanting reproducible re-runs without
   re-hitting the API, but it is a 2026-08-06-stale API response cache
   checked into history - worth a conscious call on whether that's meant to
   stay committed long-term or was committed as a one-time convenience.
   `cme_daily_features.json`, described identically as regenerable, is
   correctly NOT committed - so the two caches are handled inconsistently
   with each other despite matching docstring language.
3. **`probe_binance_aggtrades.py` and `probe_binance_trades.py` undersell
   themselves in their own docstrings** ("Exploratory: this is not part of
   the fingerprint build" / no such disclaimer needed either way) - both are
   load-bearing library imports for five-plus other scripts including the
   still-live `pair_harness.py` and `build_cadence.py`/`build_targets.py`.
   Worth relabeling if anyone reads the docstring as license to delete them.
4. **`tick_composition_ratios.py` is the one genuinely ambiguous verdict** in
   this whole set - it has a real Rust twin already
   (`GenType::Measure12a`-adjacent, `brokkr run mogwai -- tick-composition`)
   the way `roll_estimator.py` does, but its own doc frames the Python side
   as "a report" rather than an independent-algorithm conformance leg the way
   `roll_estimator.py`/`spread_conformance.json` explicitly is. If it's a
   report generator, it's ABSORB; if it's meant to stay an independent
   cross-check like roll_estimator, it's KEEP. This wants an owner decision,
   not a triage guess.
5. **The report's `mnq_fit.py` line estimate is off by more than rounding**:
   section 0.2 says "about 8,900 lines" is the base for the 10/15/75 percent
   split, but the file measures 9,515 lines on disk today (roughly 7% larger).
   Doesn't change the percentages meaningfully but the crate-scope estimate
   should be re-run against the current file, not the cited figure, before
   committing to a size budget for the rewrite.
