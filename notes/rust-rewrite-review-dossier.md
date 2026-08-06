# Rust rewrite review dossier

The single document the codex program-level review pass is pointed at.
Assembled at the close of phase 4a (`notes/rust-rewrite-phases.md`), before
the Python retires. A map, not a story: every claim below cites the file,
commit or landing record it comes from. Full narrative lives in the phase
landing records; this document does not repeat it, only indexes it.

Companion reading: `notes/rust-rewrite-phases.md` (phase-by-phase landing
records), `notes/python-script-triage.md` (per-script scope rulings),
`notes/todo.md` (parity-frozen defect and owner-decision entries),
`DATA-PURCHASE-REPORT.md` (program context for any adjudication).

## 1. Program shape

Owner-defined 2026-08-06 (`notes/rust-rewrite-phases.md:1-26`). Absorbs the
measurement/fit/synthesis method from `analysis/` into a new `mogwai-lab`
crate, in phases each gated on a committed artifact, Python originals kept
runnable beside the port until phase 4b.

| Phase | Scope | Commit(s) | Gate |
|---|---|---|---|
| 0 | Carve `mogwai-cli` out of `mogwai-server` | `86f113b` | `brokkr check --gate` green, `target/release/mogwai` exists |
| 1 | `mogwai-lab` corpus layer: stream contract, session/segment math, preflight, storage policy | `307d6a7` | `mogwai preflight` reproduces `analysis/out/mnq-fit-preflight.json` exactly |
| 2a | Deterministic kernel + unified Blocks 1-4 session engine | `c750768` | typed-canon vs `mnq-measure12a-observed.json` (observed) and `measure12a-cache/` (generated), cost excluded |
| 2b | Aggregation/inference: monthly, bootstrap, ladder, envelopes | `7511a20` | reproduces artifact's bootstrap/ladder sections from 2a's records |
| 2c-i | Assembly + validators | `8ec9e11` | assembles from caches, matches artifact minus `cost`/`binding.harness_tree_commit` |
| 2c-ii | Live run: sampler, observed pass, `mogwai measure` | `68c6ded`, fix `0cf2d49` | golden gate PASSED from clean HEAD `0cf2d49` (recorded in phase doc) |
| 2c-iii | Retirement of `crates/mogwai-cli/src/measure12a.rs` twin | `31f1d27` | 9 phase-2a gates + 2b + 2c-i re-pass; `python3 analysis/mnq_fit.py cost12a` end-to-end |
| 3a | `characterize`, `build_cadence`, `check_cadence_feasible`, `build_fingerprint` | `0c7e3ea` | 3 artifact-reproduction gates + 24 lab unit tests |
| 3b | `fit_session_profile`, protocol-11 fit + CRN solve | `c850b12` | `mnq-fit.json` reproduction, `fit_session_profile` live-archive gate, solve invariants |
| 4a | Reviewable-state prep (this phase) | - | docs/AGENTS.md updated, dossier assembled, dissolution mapping verified |
| 4b | Post-signature retirement (not started) | - | codex signature required first |

All landings by SONNET except 2a/2b/3b (OPUS, per the silent-rot/refusal-
ownership/CRN-invariant risk the owner assessed at plan time).

## 2. Pinned cross-language conventions

Each found by a parity gate disagreeing, not by reading the Python source.

- **`kernel::py_sum`** (`crates/mogwai-lab/src/kernel.rs`) - CPython's builtin
  `sum()` over floats is Neumaier-compensated since 3.12; a naive fold
  reproduces point estimates but lands bootstrap `se` ulps off, propagating
  through the simultaneous critical value into every interval in every
  family. Used at every Python `sum(...)`-over-floats site: `stdev_ddof1`,
  count-substitution weight sums (2b), `hour_vol`/`avg_curves`/
  `EventStats::report` (3a), `fit_intensity_hour`'s day-factor sum, `pooled`'s
  `mid_return_sumsq`, `generated_evidence`'s wall-time `sumsq` (3b). A Python
  `+=` loop stays a naive fold (semantically different from `sum()` in
  CPython itself). First found: phase 2b landing (`7511a20`).
- **Insertion-ordered accumulation** - several dict-walking float
  accumulations depend on Python's dict insertion order; sorting them moves
  the last ulp. `aggregate::monthly::PooledHist` is an insertion-ordered map,
  not a `BTreeMap` (2b, moves `counterfactual_exceed_968`'s last ulp);
  `EventStats::children_hist` in `cadence.rs` documents the same choice for
  2c-owned code, contrasted against `characterize.rs`'s tick-count modal tie,
  which IS safe as a `HashMap` because only sorted-key reads happen
  downstream.
- **`float_roundtrip`** (workspace-wide serde_json feature) - serde_json's
  default float parser is not correctly rounded; it lands one ULP off on
  values the 12a records actually carry. Found by the 2a gate (`c750768`).
- **`py_float_repr`** (`kernel.rs`) - the one CPython-compatible shortest
  round-trip float `repr`. `subcontract.rs` had carried a second hand-written
  copy with the wrong exponent-switch threshold; deleted in favour of the
  kernel's, frozen subcontract hash unchanged (2a).
- **`kernel::py_int_div`** - CPython's `int / int` is correctly rounded from
  EXACT operands; `a as f64 / b as f64` pre-rounds the numerator to binary64
  before dividing. Used at the fit's three nanosecond gap-sum division sites
  (order 2e16, past 2^53) where a naive cast landed one ulp off (3b,
  `c850b12`).
- **`build_fingerprint.py`'s `rng()`/`rng_typed`** - Python's dynamically
  typed `min()`/`max()`: an all-integer input list keeps `min`/`max` as JSON
  integers, only `statistics.median`'s true division always yields a float.
  `rng_typed` operates over `serde_json::Value` to preserve this; the
  plain-`f64` `rng` wrapper covers the majority already-float case (3a).
- **`fit::mtrand`** - a from-scratch port of CPython's Mersenne Twister
  (`init_by_array` seeding, `getrandbits`'s word layout, `_randbelow`'s
  rejection sampler, `choice`), pinned against CPython's own stream. Needed
  because `minute_range_envelope` draws 22,000 session labels under
  `random.Random(1)` and the draw IS the bound the minute-range gates judge
  against (3b, `c850b12`).
- **Typed-canonical serialization** (`kernel::typed_canon`) - the comparator
  every parity gate uses: compares JSON values with type fidelity (int vs
  float) rather than after a lossy round-trip, so an accidental int/float
  disagreement cannot hide inside a numerically-equal comparison (2a).
- **The nano-unit log-mid arithmetic** - the 12a engine works in integer nano
  price units end to end rather than floating mid-prices, avoiding a whole
  class of rounding divergence the Python's float path is exposed to (2a).

## 3. Parity gates and results

All `#[ignore]`d, excluded from `brokkr.toml`'s complete profile by the
`parity12a_`/`parity3a_`/`parity3b_` name prefixes (`brokkr.toml:60-74`)
because they need corpus/archive state on disk.

| Gate file | Reproduces | Result | Exclusions (each independently verified) |
|---|---|---|---|
| `mogwai-cli/tests/parity12a.rs` | Observed (22 sessions, 83s) + 8 generated seeds (~26s ea) | PASS | none |
| `mogwai-cli/tests/parity12a_aggregate.rs` | 2b's bootstrap/ladder sections | PASS, 2.2s | none |
| `mogwai-lab/tests/parity12a_i.rs` | Assembly from caches | PASS, ~6.5s | top-level `cost`, both live-cost classes |
| `mogwai-cli/tests/parity12a_ii.rs` (two tests) | Live `mogwai measure` run | PASS (golden gate from clean HEAD `0cf2d49`) | top-level `cost`, `binding.harness_tree_commit`, each seed's `cost` |
| `mogwai-lab/tests/parity3a.rs` (fingerprint) | `fingerprint.json` | PASS, one leaf excluded BY NAME (drift, see 8) | `empirical_ranges.modal_tick.max` |
| `mogwai-lab/tests/parity3a.rs` (cadence) | `cadence.json`, live 3-archive (~230M rows) run | PASS, ~69s | `provenance.generated_utc` |
| `mogwai-lab/tests/parity3a.rs` (cadence-feasible) | structural verdict | PASS (`PROCEED`) | Markov density re-simulation not ported (see 5) |
| `mogwai-cli/tests/parity3b.rs` | `mnq-fit.json`, 132/132 walk-cache hits | PASS, 82s | `binding.harness_tree_commit`, `binding.subcontract_hash`, `binding.preflight_artifact_hash` (all confirmed stale-input drift, see 8) |
| `mogwai-lab/tests/parity3b_session_profile.rs` | `fit_session_profile.py preflight`/fit, live NQ archive | PASS, 2s | none (only `session.dow_weight` is reproducible from this archive today; see 8) |

Every exclusion above was verified by direct comparison against a live
Python run at port time, never assumed honest by construction.

## 4. Parity-frozen defects awaiting 4b

Deliberately mirrored bugs, kept in both implementations so the parity gates
stay honest. Full text in `notes/todo.md`; summarized here for the reviewer.

- **Unguarded numeric conversions in the TBBO stream contract**
  (`analysis/mnq_fit.py`'s `parse_stream` and `crates/mogwai-lab/src/stream.rs`):
  price/size/bid_sz/ask_sz conversions carry no named refusal, so a malformed
  non-integer field crashes instead of refusing. Mirrored deliberately;
  decide whether it joins the refusal contract once the Python retires.
  Found phase 1.

No other parity-frozen defect entries exist in `notes/todo.md` as of this
writing; phases 2-3b added none to that class.

## 5. Documented scope gaps

- **`check_cadence_feasible.py`'s Markov density re-simulation** - the
  default (no-flag) CLI path's 3,000,000-event density re-simulation
  (`simulate_markov`, drawing from `random.Random(42)` through
  `weibullvariate`/`math.gamma`) was NOT ported. `next_count` and the
  structural `verdict()` - what the phase-3a brief calls binding, "the L0
  structural-proceed verdict" - are exact ports and gated. The stochastic
  recheck is a secondary diagnostic in the Python itself (never gates the
  structural verdict). Recorded as a real scope gap, not a rounding
  convention, in the phase-3a landing record; a full port needs
  `fit::mtrand`-class Mersenne Twister work plus a `weibullvariate`/`gamma`
  port if a later phase wants it.
- **`build_fingerprint.py`'s `findings.md` side artifact** not ported - a
  human-readable report, not gated by anything (3a).

## 6. Spec-thinness notes from phase 2b

Recorded in the phase 2b commit message (`7511a20`) but NOT copied into
`notes/rust-rewrite-phases.md`'s own 2b landing text - captured here so the
codex pass sees them:

- The fail-closed ladder's **completeness gates on rung 4a and rung 5a live
  in CODE beyond what `notes/protocol-12a-measurement-spec.md` section 6.2
  states in prose** - the port is faithful to the code, but the spec text
  alone would not have specified them.
- **Rung 1 records an unpaired `a_print_excess` beside a paired `fired`** in
  the committed artifact (`analysis/mnq-measure-12a.json`, `child_walk`
  subcheck pair `{a_print_excess, b_mid_clean}` per
  `notes/protocol-12a-measurement-spec.md:1282` and
  `crates/mogwai-lab/src/aggregate/ladder.rs:440`) - reproduced verbatim,
  not investigated further; flagged for the codex pass rather than resolved
  silently.

## 7. Accidental-agreement discoveries

Both found and recorded in the phase 2a commit message (`c750768`); phase 1
(`307d6a7`) records the same segment-math unification effort from the other
side.

- **Floor-division vs refusal**: the 12a engine works in integer nano price
  units end to end and REFUSES off-grid divisions where `mnq_fit.py`
  floor-divided. Both parity gates prove the divide is exact on every
  reachable input in the observed corpus and all eight generated walks, so
  the stricter Rust behavior never actually diverges from the Python's
  floor-division on real data - an accidental agreement, not a proof the two
  are equivalent in general.
- **Segment order**: the port keeps Python's INSERTION order for segments,
  where the OLD Rust twin (the pre-rewrite `measure12a.rs` consumer-only
  module) had hard-coded a different order. This is a live latent divergence
  between what the two Rust implementations would have produced, unreachable
  in the July corpus and all eight walks - so it never showed up as a test
  failure, only as a fact worth recording for anyone who later feeds the
  engine data shaped differently than the fitted corpus.

## 8. Drift findings awaiting owner decisions

- **`fingerprint.json`'s `empirical_ranges.modal_tick.max`**: committed value
  `0.25` (the exact ceiling MNQ's tick sits on); regenerating from today's
  gitignored `char_*.json` inputs - confirmed in BOTH the unmodified Python
  and the Rust port - yields `0.1`. The `char_*.json` files were regenerated
  locally after the commit that produced `fingerprint.json`, moving the
  anchor's modal tick without anyone re-running `build_fingerprint.py`. Not a
  port defect. `fingerprint.json` is compiled into the generator via
  `include_str!`, so re-committing it is a `TAPE_PROTOCOL_VERSION` decision.
  Owner decision recorded in `notes/todo.md`. Found phase 3a.
- **`mnq-fit.json`'s two binding hashes**: `binding.subcontract_hash`
  (artifact: `35e5b033...`) and `binding.preflight_artifact_hash` (artifact:
  `adf6b8e7...`, hashing a preflight file that today reads `96013588...`) are
  both stale relative to today's tree, because the protocol-12a constants
  joined `SUBCONTRACT_KEYS` after the protocol-11 fit ran. Running
  `mnq_fit.py`'s OWN `subcontract_hash()` today returns `1ca79d9c...`,
  byte-identical to what the Rust port computes now AND to what the
  committed `mnq-fit-preflight.json` already records - so this is the same
  stale-input-drift class as the fingerprint finding above, not a port
  defect, and both are cross-verified against a direct Python run. Recorded
  alongside the fingerprint drift as a re-commit decision. Found phase 3b.

## 9. Deviations from standing process

Owner-authorized for the duration of this program: no codex sparring during
the build (codex capacity reserved for the single program-level review pass
phase 4 gates on), LSP diagnostics suppression, and agents self-checking
their own landings rather than a second-agent review per slice. Recorded
here because none of it is otherwise written down, and a reviewer comparing
this program's process against the repository's normal review cadence needs
to know it was a deliberate, bounded exception rather than an oversight.

## 10. Open owner decisions

All recorded in full in `notes/todo.md`; indexed here.

- **`select_windows.py`** - absorb with a `targets-frozen.json` gate (one
  small slice; `session_profile.rs` already lands most of the needed
  infrastructure), or re-sentence to KEEP until a sampling-frame purchase
  question returns. Assessed, not decided, phase 3b.
- **`tick_composition_ratios.py`** - reading it settled the triage's own
  ambiguity: no independent estimator, a report generator over Rust-produced
  fixtures. ABSORB as a `--report` mode on the existing `tick-composition`
  subcommand is the reading the code supports; the frozen baseline tables
  must move as data, never re-derived. Assessed, not decided, phase 3b.
- **The two drift re-commits** (section 8 above): whether to re-run
  `build_fingerprint.py`/re-commit `fingerprint.json` accepting the `0.25`
  to `0.1` modal-tick change, and whether to re-commit `mnq-fit.json`'s
  binding hashes to their current values. Both are "do nothing until ruled"
  in `notes/todo.md`.

## 11. `test_characterize.py` dissolution mapping

Verified by direct correspondence, not by re-reading the phase-3a landing
claim. `analysis/test_characterize.py` carries 31 tests across seven
`unittest.TestCase` classes; the file itself is UNCHANGED (deletion is a
4b action). Mapping:

| Python class (count) | Rust counterpart | Status |
|---|---|---|
| `BinningTests` (2) | `crates/mogwai-lab/src/characterize/tests.rs`: `bins_are_monotone_and_cover_the_support`, `half_and_five_land_in_different_bins` | Complete, 1:1 |
| `QuantileTests` (3) | same file: `quantile_returns_the_geometric_bin_centre`, `quantile_picks_the_bin_holding_the_rank`, `an_empty_histogram_has_no_quantile` | Complete, 1:1 |
| `VisitClosureTests` (7) | same file: all 7 assertions present under matching names | Complete, 1:1 |
| `EraWindowTests` (4) | same file: all 4 assertions present under matching names | Complete, 1:1 |
| `ReportTests` (2) | same file: `the_normalizer_is_the_era_windowed_size_median`, `dispersion_is_the_p90_over_p50_of_its_own_histogram` | Complete, 1:1 |
| `LevelQueueTests` (7) | `crates/mogwai-lab/src/fingerprint.rs` tests module: all 7 assertions present | Complete, 1:1 |
| `CadenceTests` (6) | see below, mixed | Partial before this phase; completed below |

`CadenceTests` breakdown:

- `test_event_grouping_rules_are_distinct` -> `cadence.rs::event_grouping_rules_are_distinct`. Covered.
- `test_mixture_solution_and_fallback` -> `cadence.rs::mixture_solution_and_fallback`. Covered.
- `test_geometric_sampler_uses_the_pinned_inverse_cdf` -> `cadence_feasible.rs::geometric_sampler_uses_the_pinned_inverse_cdf`. Covered.
- `test_committed_cadence_is_loadable` (`load_cadence()["anchor"] == "BTCUSDT"`) -
  had NO Rust counterpart; the phase-3a landing record does not claim one.
  **Added this phase**: `fingerprint.rs::the_committed_cadence_is_loadable`.
- `test_raw_probe_returns_structured_result` (a synthetic 3-row Binance
  trades zip, asserting the 2-event grouping distinction and
  `per_second_counts`'s presence) - the phase-3a landing record claims this
  is "covered live by gate 2 instead of a synthetic zip fixture"
  (`parity3a_cadence_matches_the_committed_artifact`, the real 3-archive
  run). That gate exercises `cadence::probe` on real data but never pins the
  specific small-N grouping-distinction shape the Python unit test isolates.
  **Added this phase**: `cadence.rs::probe_returns_structured_result_over_a_synthetic_fixture`,
  a byte-for-byte port of the Python fixture and assertions.
- `test_kline_probe_returns_structured_result` (`probe_binance_klines.py`) -
  no counterpart, none expected: `probe_binance_klines.py` is triaged DEAD
  (`notes/python-script-triage.md`), never in the ABSORB set.
- `test_aggtrades_probe_returns_structured_result` (`probe_binance_aggtrades.py`)
  - no counterpart, none expected: `probe_binance_aggtrades.py` is triaged
  KEEP (a live library import for `pair_harness.py` and others), never in
  the ABSORB set.

Net: of the 31 Python tests, 27 assert behavior inside the ABSORB set and
all 27 now have a verified Rust counterpart (2 added this phase); 4
(`test_kline_probe_returns_structured_result`,
`test_aggtrades_probe_returns_structured_result`, and their probe modules)
assert behavior in scripts the triage kept Python, correctly outside the
dissolution's scope.

Lateral: the phase-3a landing record's own test count ("17 tests" for the
Binning/Quantile/VisitClosure/EraWindow/Report group, "24 lab unit tests"
overall in the commit message) does not match a direct count of the Rust
test file, which carries 18 tests in that group (2+3+7+4+2) and 20 total in
`characterize/tests.rs` (2 more not ported from Python: `decimals_used_strips_trailing_zeros`,
`log_bin_clamps_at_the_ends_and_is_monotone`). Not a coverage gap - every
Python assertion IS present - just a stale count in the landing prose,
worth a correction pass in phase 4b's own record if anyone edits that
section again (not touched here per the phases-doc no-deletion constraint
extending, by the same spirit, to not rewriting past landing narrative).
