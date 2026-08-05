# MNQ July TBBO fit: static seams, protocol 10

Implementation specification, 2026-08-05, revision 5: two full
document reviews, then the protocol-version correction (the shipped
constant is already 9; this fit lands at 10). Written against `reference/technical-implementation-spec.md`
(the contract this document must satisfy) and spawned from
`DATA-PURCHASE-REPORT.md` section 0.1 resume-order item 2 and section
9.7 wave 2B (the TODO source). Scope, estimator definitions, gates and
the execution findings of revision 2 were argued to consensus by both
reviewers on 2026-08-05, under the owner's standing delegation; the
owner gates exactly two things, money (none is spent here) and the
first run of any newly produced script (sequenced explicitly below).

This is a `notes/`-class document: transient, no truth guarantee,
nothing durable may cite it. When the landing is complete, what remains
true lives in the preset provenance, `reference/`, and the fit artifact.

---

## 1. The goal

Fill the MNQ preset's declared-pending generator slots with values
fitted from the delivered July 2026 MNQ TBBO month, flip their
provenance to `fitted`, propagate the change loudly through MES
inheritance, bump `TAPE_PROTOCOL_VERSION` 9 -> 10 in the same indivisible
landing, verify the affected goldens, and remeasure the
tick-composition budgets under a new 9-to-10 comparison mode. Static
seam values only: the dynamic volatility response of purchase-report
item 14.4-A is a separate later spec with its own bump.

## 2. Survey of the ground

### 2.1 The evidence

`research/market-data/databento/mnqv/2026-07.full.tbbo/` (gitignored),
job `GLBX-20260805-HAPEWPABKG`, five files hash-verified by the
downloader; the ledger entry in `analysis/databento-jobs.json` is
terminal at `downloaded` and carries the sha256 inventory. Two data
files: `glbx-mdp3-20260630.tbbo.csv.zst` (the July 1 session's Sunday
open pulled in by the UTC session bounds) and
`glbx-mdp3-20260701-20260731.tbbo.csv.zst` (914 MB). TBBO is one record
per trade with the pre-trade top of book attached: price, size, side,
nanosecond timestamps, bid/ask price and size, instrument id, and the
continuous-label symbol echo (`MNQ.v.0` - the symbol column is NOT the
contract; `instrument_id` is the sharper witness, purchase report 0.1).
The DBN side alphabet is B=buy, A=SELL, N=none
(`research/dbn/rust/dbn/src/enums.rs`); reading B/S manufactures
unsided rows.

The book is observed only at trade instants. That matches the
generator's observation process - protocol 7 publishes one pre-parent
book and holds it across children - and is why every quote estimator
below is parent-weighted, never row-weighted. No mitigation can turn
TBBO into an independent quote clock; the limitation is recorded, not
worked around.

### 2.2 The target artifact

`crates/mogwai-server/presets/mnq.toml` `[instrument.generator]`. Slots
and their disposition:

| slot | disposition |
|---|---|
| `mean_event_duration_s` | fitted, cadence family |
| `children_mean` | fitted, cadence family |
| `children_single_frac` | fitted, cadence family |
| `levels_mean` | fitted, cadence family |
| `size_round_frac` | conditional: unidentifiable branch expected, see 4.3 |
| `latent_size_median` | fitted, inverse solve through `materialize_size` |
| `start_price` | fitted, terminal anchor |
| `vol_scalar` | fitted, inverse calibration through the generator |
| `quoted_width` | fitted, parent-weighted mode |
| `top_sizes` | fitted, nearest-rank medians |
| `trade_displacement_ticks` | fitted, inverse calibration |
| `modal_tick`, `price_decimals`, `symbol` | untouched, contract spec |

The session arrays and calendar are untouched: the 2020-2026 NQ-bar fit
stands (wave 1 measured count-vs-volume peak-to-trough ratio 0.95 over
ten July sessions; this month's sessions re-measure it as a diagnostic,
section 4.8, never as a profile mutation).

### 2.3 What depends on the values

- `GeneratedSource` construction validates scalars
  (`source.rs`, `fingerprint.rs`); the fitted values must pass the
  mechanism gates (native-unit size coherence, decimal
  representability, sweep relationships, volatility headroom).
- Goldens, surveyed: `clean_regime_is_byte_identical` constructs the
  XBTUSD fingerprint anchor DIRECTLY and never resolves the MNQ or MES
  presets - this fit MUST leave it byte-identical, and its remaining
  unchanged is itself a gate. The fill goldens (`fill_golden.rs`) run
  on the BTCUSDT profile and are likewise expected unchanged. The
  three `mogwai-data` seam tests ALSO do not resolve presets:
  `a_quote_precedes_every_parent_burst` uses
  `GeneratorScalars::xbtusd_anchor`,
  `synthetic_spread_decomposition_at_protocol_seven` uses the anchor
  plus a hard-coded MNQ-shaped grid, and
  `the_trade_displacement_never_varies` uses those two constructed
  profiles and asserts the module constant - fitting `mnq.toml`
  changes none of their inputs, so all three MUST remain unchanged as
  well. The preset-dependent surface that actually moves is in
  `mogwai-server`: the shipped-preset provenance test and any test
  asserting MNQ or MES effective values. Brick L re-blesses exactly
  that set and ADDS the preset-resolution assertions the landing
  needs (named in Brick L), since no existing test pins fitted MNQ or
  inherited MES effective values directly.
- MES: `mes.toml` is `preset = "MNQ"` plus five overrides. Every
  fitted MNQ generator value flows into effective MES. This is a
  DECIDED, loud inheritance (section 6), not an accident.
- `TAPE_PROTOCOL_VERSION` (`mogwai-data`): any of these values changes
  the stream. The version bump and the value change are ONE landing
  (brick L); no kept or gated tree state may contain fitted preset
  values under protocol 9. (Protocol 9 is ALREADY SHIPPED: commit
  5fc974d split parent advancement from wire materialization,
  byte-identical tape, unconditional rule still bumped the constant.
  The purchase report's present-tense protocol-8 claims predate that
  commit and are corrected in Brick D.)
- Tick-composition budgets (`CHECKPOINT_K`, `SWEEP_DRAIN_BUDGET`,
  `MAX_WARMUP_MATERIALIZATION_TICKS`, `fanout_depth`): resized at
  protocol 8 from measured composition. Remeasurement is UNCONDITIONAL
  here, and `analysis/tick_composition_ratios.py` needs a NEW mode for
  it: the existing `projection` (6 vs 7) and `independent` (7 vs 8)
  modes are version-pinned with frozen historical baselines. The
  before side needs a REAL protocol-9 fixture: 5fc974d corrected the
  protocol-8 fixture in place as the composition a protocol-9 report
  must match exactly (byte-identical tape), but a mode whose name,
  version assertions and fixture metadata disagree would embed an
  exceptional rule in the script forever. Brick B0 therefore first
  regenerates `analysis/tick-composition-protocol-9.json` with the
  existing instrument, on the clean shipped tree before any landing
  work, and asserts it exactly equals the protocol-8 fixture
  excluding only `tape_protocol_version` and `pairing_id` - verifying
  the byte-identity claim on the current implementation - and
  `independent_9_10` compares that against protocol 10. Brick R
  builds the mode and the identity verifier.

### 2.4 Instruments: what exists and what must be built

- The pair harness (`analysis/pair_harness.py`) froze the estimator
  lineage this spec reuses: contiguous timestamp-plus-side parent
  grouping (never merging non-contiguous equal timestamps), session
  assignment against the CDT calendar, halt and session-boundary gap
  exclusion, the unsided-share gate, csv.zst streaming, and
  input-identity binding (ledger + manifest + rehash). The new harness
  imports or re-implements these under conformance checks that pin
  equality with the frozen behavior; it does not fork their semantics.
- `mogwai gen` CANNOT serve as the inverse-calibration instrument as
  it stands. Verified against `gen.rs`: it accepts `--symbol` (builtin
  venue, then embedded preset), `--seed`, `--start-price`, `--regime`,
  `--havoc` - no config or profile path, and `resolve_profile` reaches
  only `profile_from_preset`. A scratch profile with candidate scalars
  is unreachable, and CSV output at MNQ cadence (a simulated month is
  order 10^8 trades) is the wrong shape for a candidate search loop.
  Brick G builds the instrument BEFORE any fitting brick: a
  `--config <path>` mode resolving through the server's real config
  loading, and a summary output mode that consumes the full generator
  walk (every draw: sizes, prices, sides, quotes) while emitting only
  the statistics the calibration needs.

## 3. The order of work

One implementation spec containing a frozen measurement sub-contract.
Brick sequence, with the owner gate placed explicitly:

1. Brick G (Rust instrumentation for `gen`), gated by its own named
   tests - buildable and testable with no new-script run.
2. Brick R (the 9-to-10 ratio mode and the 8/9 identity verifier),
   gated by its selftest; the mode is inert until a protocol-10
   fixture exists. Then Brick B0: the protocol-9 baseline fixture is
   measured on the clean shipped tree and identity-verified against
   protocol 8 - BEFORE any landing work, because the pre-landing
   state is gone once Brick L is built.
3. Bricks H1-H2 (the Python harness and its conformance fixtures)
   are BUILT UNCOMMITTED: a brick is kept only on a verified gate,
   and the harness's gate is its own selftest, which cannot run
   before the owner gate. Nothing runs against real data.
4. Brick F (freeze): the sub-contract constants and artifact schema
   are final; both reviewers sign off on the constants block.
5. OWNER GATE: `analysis/mnq_fit.py` is a newly produced script. Its
   FIRST run - including `selftest` - happens only after the owner
   clears it. Until then no brick below starts.
6. Brick M1 (`selftest`) is the keep gate for H1-H2: the harness
   commits only after its selftest passes. Bricks M2-M3 (preflight,
   fit) follow; the hash-bound artifact `analysis/mnq-fit.json` is
   produced and read.
7. Brick V (representability verdicts, family-isolated).
8. Bricks L and B as ONE keep/revert unit: the landing is built
   (values + provenance + MES + protocol 10 + new preset-resolution
   tests), Brick B remeasures composition on that UNCOMMITTED tree,
   and the whole unit - including any ceiling resize - is committed or
   abandoned together. Then Brick D (documentation).

## 4. The frozen measurement sub-contract

Every rule below is fixed before the first real-data run and encoded as
a named constant or function in the harness. The artifact records the
sub-contract's own hash.

### 4.1 Input contract and preflight (fail-closed)

Identity, before a byte of CSV is read:

- The ledger entry for `mnqv|2026-07.full|tbbo` must be `downloaded`,
  its job id `GLBX-20260805-HAPEWPABKG`, and the on-disk bytes must
  rehash to the ledger's sha256 inventory and the landing manifest.

Stream contract, over the two files read in date order as ONE stream:

- Timestamps uniformly 19-digit nanosecond epochs; ordering monotone
  non-decreasing ACROSS the file boundary, not merely within each
  file; zero regressions; the files may neither overlap nor duplicate
  rows at the boundary (refused, not deduplicated).
- Grid: trade price, bid price and ask price each exactly on the 0.25
  grid (fixed-precision ints per the decided submit flags); any
  off-grid value refuses.
- Side alphabet strictly B/A/N; unsided share above
  `MAX_UNSIDED_SHARE = 0.01` refuses.

Book classification, before any ordering assertion: every row's book
is classified `normal` (ask > bid), `locked` (ask == bid), `crossed`
(ask < bid), or `nonpositive` (bid <= 0 or ask <= 0). There is no
unconditional `ask >= bid` requirement - the classes ARE the handling:

- Combined invalid-width share (locked + crossed + nonpositive) above
  `MAX_INVALID_WIDTH_SHARE = 0.001` refuses; per-session shares are
  reported. A locked single-venue futures book is not a valid top of
  book and counts as invalid here.
- Invalid-book rows are excluded from quote estimators and INCLUDED in
  cadence and size. A parent whose FIRST row carries an invalid book
  has no quote observation - a later child's book is post-trade and is
  never substituted.
- Valid parent-quote coverage below
  `MIN_VALID_PARENT_QUOTE_SHARE = 0.95` refuses: locked-heavy data
  must not silently hollow out the quote evidence while preflight
  passes.

Contract identity and sessions:

- Purity is EXACT for this no-roll month:
  `MIN_DOMINANT_ID_SHARE = 1.0`. A session whose rows resolve to more
  than one `instrument_id` is excluded by name; more than
  `MAX_EXCLUDED_SESSIONS = 4` such exclusions refuses. The symbol
  column (continuous echo) is never the witness.
- The session inventory is FROZEN as a table in the harness, not
  inferred from the weekly calendar (which cannot encode holidays):
  23 weekday session labels July 1 through July 31; July 3 excluded as
  the Independence Day early close; 22 expected full sessions. Fewer
  than `MIN_USABLE_SESSIONS = 18` usable refuses.
- Sessions are assigned against the shipped CDT calendar with the
  session-fit early-close convention.

On pass, preflight persists an artifact bound to input hashes and the
sub-contract hash; `fit` requires and re-verifies it.

### 4.2 Cadence family (pooled, parent-inferred)

Parents: maximal contiguous runs sharing nanosecond timestamp AND
aggressor side, per the frozen pair-harness rule; unsided rows never
enter grouping. Parent and child counts are cross-checked against an
independent second implementation in the harness (two implementations
of one rule police each other, as the pair harness did).

- `mean_event_duration_s`: pooled mean of eligible inter-parent gaps;
  gaps crossing a halt or session boundary excluded.
- `children_mean`: total sided rows / total parents.
- `children_single_frac`: single-row parents / total parents.
- `levels_mean`: total distinct price levels per parent / total
  parents.

Pooled values are the estimates (matching the generator's
event-weighted scalar semantics); per-session values, median, IQR, min,
max are reported as stability diagnostics only.

### 4.3 Size family

All valid trades enter the size histogram, unsided included: side and
book validity are not properties of the size process, so an unsided
print's size is still size evidence and a broken quote beside a trade
does not invalidate the trade's size. The generated population has no
unsided or invalid-book class by construction; the preflight caps (1%
unsided, 0.1% invalid-width) bound how far the two populations can
drift. The artifact states this explicitly: `size_population` carries
the population definition and the print/sided/unsided/valid-book/
invalid-book counts, so the verdict reader sees the asymmetry and its
bound instead of discovering it in the code. The hour curves and the
terminal price anchor share the convention (a data diagnostic
classifies nothing; the anchor needs a price, not a side).

`latent_size_median`: deterministic inverse solve through the exact
`materialize_size` transformation (floor at `min_size = 1`,
half-away-from-zero rounding, lot snap at `integral_lot`), with
`SIZE_LOG_SIGMA` fixed (shared shape, stopping rule). Common random
numbers across candidate medians; scored against the complete discrete
histogram on: max empirical-CDF distance, mean, p50/p75/p90/p95/p99,
and mass at the one-contract floor.

`size_round_frac`: identifiability branch, decided by the fitted
median. If `latent_size_median < 10`, `integral_lot = 1` and both
snap branches round to whole contracts: the parameter is structurally
INERT on this grid. Then: no fitted claim; the declared value stays;
its rationale is rewritten to state the unidentifiability; the
artifact records the branch. If the fitted median reaches 10+, it is
estimated jointly with the median through `materialize_size`. Observed
mass at decade multiples is never used directly - rounded lognormal
draws land there anyway.

### 4.4 Quote family

One observation per inferred parent: the first row's pre-trade book,
valid books only per 4.1.

- `quoted_width`: parent-weighted width histogram in ticks; the landed
  value is the parent-weighted MODE unconditionally, ties broken
  toward the smaller width. Reported: full histogram, modal mass,
  median, p90, mean absolute deviation from the selected static value.
  The static seam reproduces the mode exactly; distributional loss is
  a stated limitation, not a failed test.
- `top_sizes`: nearest-rank (never interpolated) empirical medians of
  bid size and ask size separately, always an observed whole-contract
  value. Full distributions and tail quantiles reported.

### 4.5 Displacement

Observable, per parent with a valid quote observation, in ticks: buyer
`(first_trade - quote_mid)`, seller `(quote_mid - first_trade)`, each
divided by the tick. Reported: complete signed distribution, wrong-side
fraction, inside-mid / at-touch / beyond-touch fractions, median and
p90, buyer and seller separately.

The scalar is NOT the empirical median: the configured value acts
before grid rounding and interacts with compatible repeats.
`trade_displacement_ticks` is inverse-calibrated through the generator
(Brick G instrument) with the fitted width already installed, so the
GENERATED parent-level effective displacement median matches the
empirical parent-weighted target. Materiality of the wrong-side
population is a named threshold: a wrong-side share above
`MAX_WRONG_SIDE_SHARE = 0.05` of valid-quote parents FAILS the
displacement family's representability outright (the generator
structurally forbids wrong-side prints, so beyond that share the
static model misdescribes the data); at or below it, the share is
recorded as a stated limitation and the fit proceeds on the full
signed distribution.

### 4.6 Start price

The last valid trade price of the final usable session, deterministic
by input order after validation, already on the grid. An observed
terminal anchor, not a fair value; the definition lives in the artifact
and the preset comment (see 5, Brick L, provenance note).

### 4.7 Volatility

`vol_scalar` is the unconditional scale of one GARCH update, advancing
once per parent event; a wall-time target would conflate volatility
with cadence. Target: pooled RMS of adjacent inferred-parent quote-mid
log returns (halt and boundary crossings excluded identically on both
sides). Inverse calibration runs the full generator through the Brick G
instrument with shared GARCH constants fixed, fitted cadence installed,
the July calendar and session profile active, frozen seed set and
burn-in. Fixed-horizon realized vol at 60 s and 5 min is reported as a
secondary diagnostic only.

### 4.75 The inverse-solve contract (shared by 4.3, 4.5, 4.7)

Two implementers must produce the same artifact, so every solve pins
domain, algorithm, termination, ordering and failure:

- **Algorithm, ALL solves identically:** deterministic objective
  MINIMIZATION. Coarse deterministic grid over the candidate domain;
  the initial bracket is [left neighbor, right neighbor] of the
  best-scoring grid point (a boundary winner takes its single inside
  neighbor interval, clamped - the domain is never widened). Each
  trisection iteration evaluates exactly the two interior points at
  1/3 and 2/3 of the current bracket; the surviving bracket is the
  left or right two-thirds subinterval containing the best-scoring
  point evaluated so far; when scores tie, AND when the best point
  lies in the middle third that both candidate subintervals share,
  the LEFT (smaller) subinterval survives. Termination: relative step below
  `SOLVE_RELATIVE_STEP = 1e-3`, switching to an ABSOLUTE step test
  when the bracket contains zero (where a relative step is
  undefined): `SOLVE_ABSOLUTE_STEP` per parameter, 0.001 tick for
  displacement (the one domain including zero). NO monotonicity is
  assumed anywhere: grid rounding, compatible repeats, caps and path
  dependence can create plateaus and reversals. No stochastic search.
  Every candidate is evaluated on the SAME frozen SEARCH budget
  (Brick G), which makes the search deterministic end to end.
- **`latent_size_median`:** domain [0.5, 500] contracts, log-spaced
  64-point coarse grid. Objective, LEXICOGRAPHIC: (1) max
  empirical-CDF distance, (2) absolute mean difference, (3) sum of
  absolute quantile differences at p50/p75/p90/p95/p99, (4) absolute
  floor-mass difference. Ties break toward the SMALLER candidate.
- **`size_round_frac`:** the branch selection is NOT circular; the
  two branches are COMPETING MODELS solved independently and compared
  on the same lexicographic objective: model A fixes
  `size_round_frac` at its current declared value and solves the
  median alone; model B holds each of the 51 frac grid values (0.00
  to 0.50 step 0.01) FIXED in turn, refines the median independently
  to termination under each, and compares the 51 (frac, median)
  results on the lexicographic objective. The globally better
  lexicographic score across A and all of B wins; ties toward model
  A, then the smaller frac, then the smaller median. Independently of which
  model wins, if the WINNING median is below 10 the frac is
  structurally inert (`integral_lot = 1`) and is recorded as
  unidentifiable per 4.3 - model B cannot beat model A there except
  by noise, and the identifiability rule, not the score, decides the
  provenance claim.
- **`trade_displacement_ticks`:** domain [0.0, 2.0 x fitted width] in
  ticks; objective: absolute difference between the generated
  parent-level effective displacement median and the empirical
  target; terminate at `SOLVE_RELATIVE_STEP` or an objective below
  0.01 tick. Ties toward the smaller scalar.
- **`vol_scalar`:** log-domain [1e-8, 1e-4]; objective: absolute
  relative difference between generated one-parent quote-mid RMS and
  the target; terminate at `SOLVE_RELATIVE_STEP` or an objective
  below 0.001. Ties toward the smaller scalar.
- **Failure:** a solve whose best candidate still violates the
  family's representability tolerance under the FULL final budget
  (4.9) FAILS that family; the artifact records the best candidate,
  its scores under both budgets, and the verdict. No fallback
  widening of the domain after results are seen.

### 4.8 Diagnostics (findings, never gates, never mutations)

- Shared-shape check: return ACF lag 1, abs-return ACF lags 1/10/50,
  duration dispersion and ACF, `zero_change_frac` (with the 7.1
  tick-to-price account stated beside it), measured on the July MNQ
  parents and compared against the crypto-fitted module constants'
  implied values. A large divergence MOTIVATES a future spec; nothing
  here changes a shared constant.
- Count-vs-volume session curves at month scale: the wave 1
  measurement (ratio 0.95 over ten sessions) re-measured over the
  usable sessions. The fitted session profile is not mutated by any
  outcome; a materially different ratio is recorded for the provenance
  caveat.

### 4.9 Representability gate (family-isolated, target-local)

Probes are FAMILY-ISOLATED so one failed candidate cannot block
evaluation of unrelated targets: each family's probe profile carries
that family's candidates with every other slot at its current declared
value. Families: cadence (the four scalars, atomic - one failure fails
the family), size (`latent_size_median` + `size_round_frac` branch),
quote (width + top sizes), displacement, volatility, start price.
Volatility's probe additionally requires fitted cadence installed
(4.7); a cadence-family failure therefore stops the landing outright,
as intended. After family verdicts, ONE final combined profile carrying
every PASSING target receives a complete representability run; the
combined run's verdicts are the ones that land.

Tolerances (generated month against observed July, frozen seeds):

| target | tolerance |
|---|---|
| `mean_event_duration_s` | 10% relative |
| `children_mean` | 10% relative |
| `children_single_frac` | 0.05 absolute |
| `levels_mean` | 15% relative |
| size: empirical-CDF distance | <= 0.10 |
| size: mean | 15% relative |
| size: p90, p99 | larger of 1 contract or 20% |
| displacement: generated median | 0.25 tick absolute |
| displacement: buyer and seller medians | 0.5 tick of the observed SAME-SIDE median each; a side with zero valid-quote parents in the data gates vacuously and is reported as such |
| quote-mid one-parent RMS | 10% relative |
| width | exact configured integer |
| top sizes | exact configured integers |
| start price | exact scratch-profile resolution of the configured value (the generator updates before the first observable parent, so first-book displacement is a reported DIAGNOSTIC, not a gate) |

Stopping is target-local. Input, identity, timestamp, quote-integrity
or grouping failures stop the ENTIRE analysis. A target that is
unidentifiable or fails its representation tolerance stays `declared`,
named unavailable-or-misrepresented in the artifact, and does NOT land
as fitted - there is no accepted-diagnostic path to fitted provenance.
Targets that pass land together at protocol 10. If the cadence family
fails wholesale, the landing STOPS and the outcome is a
measured-failure report motivating a model-change spec - a legitimate
result of this spec, not a failure of it.

## 5. Bricks

Each brick names its gate with the exact command. The suite stays green
at every boundary.

**Brick G - the calibration instrument (Rust, `mogwai gen`).** Two
additions to `gen.rs`: (1) `--config <PATH>`, mutually exclusive with
`--symbol`, resolving a scratch instrument config through the server's
REAL `Config` loading and instrument-profile construction - the same
path a served operator config takes - so candidate scalars are
expressible without touching committed presets; (2) a summary mode
(`--type summary`) that drives the IDENTICAL generator walk - every
draw consumed: sizes, prices, sides, quotes, sweep children - and
emits only the fit statistics as one JSON object. `advance_parent()`
is insufficient (its summary discards sizes, prices, sides and quotes)
and is not used.

The summary contract, pinned: one JSON object PER SEED (pooling is the
harness's job). Every distributional field is a BOUNDED sufficient
statistic - histograms and count/sum/sum-of-squares accumulators,
never raw arrays (a month is order 10^7 parents; arrays would defeat
the summary's purpose). Fields:

- `seed` (u64), `parents` (u64), `sided_rows` (u64),
  `single_parents` (u64), `level_count_sum` (u64);
- `gap_sum_ns` (u64) and `eligible_gaps` (u64), eligible inter-parent
  gaps only;
- `size_histogram` (map decimal-string size -> u64 count);
- `bid_size_histogram`, `ask_size_histogram` (map decimal-string ->
  u64), one observation per parent quote;
- `width_ticks_histogram` (map u64 -> u64), per parent quote;
- `buyer_displacement_hist`, `seller_displacement_hist` (map from
  tick-quantized signed displacement string, bin width
  `DISPLACEMENT_BIN_TICKS = 0.05`, -> u64), per parent with a valid
  quote; wrong-side observations land in negative bins;
- `mid_return_count` (u64), `mid_return_sum` (f64),
  `mid_return_sumsq` (f64) over adjacent-parent quote-mid log
  returns, halt/boundary-excluded (RMS derives from these);
- `horizon_vol` (map of horizon-seconds string in {"60", "300"} to
  {`count`, `sum`, `sumsq`} of fixed-horizon log returns), the
  secondary diagnostics of 4.7;
- `first_book_mid` (decimal string, diagnostic only, see 4.9);
- `measured_from_ns`, `measured_until_ns` (u64, the accumulation
  bounds actually applied).

CLI inputs, pinned: `--config <path> --type summary --seed <s>
--start <ns> --length <dur> --warmup <dur>`. Warm-up and measurement
are SEPARATE bounds: generation begins at `start - warmup` with
`SUMMARY_WARMUP = 3d`; every accumulator covers exactly
`[start, start + length)` and nothing outside it, so the measurement
window is the intended calendar interval with full session weighting
and independent implementations cannot diverge on where accumulation
begins. (This replaces the earlier parent-count burn-in.) The frozen
seed list is `1..=8` (common random numbers by construction).
The two evaluation budgets are frozen numerically as harness
constants, each an exact UTC instant (the ns value is derived in the
harness and asserted in its selftest against the instant written
here):

```
SEARCH_START  = 2026-07-05T22:00:00Z   (the Monday July 6 session
                                        opens Sunday 17:00 CDT)
SEARCH_LENGTH = 7d exactly             (through 2026-07-12T22:00:00Z)
SEARCH_SEEDS  = [1, 2]
FINAL_START    = 2026-06-30T22:00:00Z  (the July 1 session open; the
                                        delivered 20260630 file exists
                                        for exactly this reason)
FINAL_START_NS = 1782856800000000000
FINAL_END      = 2026-07-31T21:00:00Z  (the Friday 16:00 CDT close)
FINAL_END_NS   = 1785531600000000000
FINAL_LENGTH   = 2674800s              (30d23h, as ONE integer+unit,
                                        the only shape --length takes)
FINAL_SEEDS    = [1, 2, 3, 4, 5, 6, 7, 8]
```

Candidate search runs on the SEARCH budget; the final
representability run (4.9) uses the FINAL budget. The split is what
keeps the 64-point grids from implying hundreds of full-month walks.

The fixed-horizon diagnostics (60 s and 300 s) are NON-OVERLAPPING
consecutive windows aligned to the measurement start; the window
return is the log difference of AS-OF quote mids (the last parent
quote mid at or before each boundary); a window containing any halt
or session-boundary crossing is excluded entirely. The same
convention applies to the observed side in the harness.

Gates, named tests to write:
`brokkr test -p mogwai-server a_scratch_config_profile_matches_the_served_profile`
(equivalence of `--config` resolution with the server path),
`brokkr test -p mogwai-server each_candidate_scalar_moves_the_summary`
(every fitted slot demonstrably reaches the measurement),
`brokkr test -p mogwai-server summary_matches_an_independent_tick_walk`
(IN-MEMORY equivalence: drive one identically seeded source through
`next_tick()`, accumulate the summary independently from the complete
`TickEvent` stream INCLUDING quotes, and assert every summary field
exactly equal - the CSV trade output omits quotes and cannot verify
this). `brokkr check` green.

**Brick R - the 9-to-10 ratio mode.** Extend
`analysis/tick_composition_ratios.py` with `--mode independent_9_10`:
before/after fixtures `tick-composition-protocol-9.json` (regenerated
in Brick B0 and byte-identity-verified against the historical
protocol-8 fixture) / `tick-composition-protocol-10.json`;
independent traversal and pairing requirements as in `independent`,
with DIFFERENT pairing ids required for the 9-to-10 comparison.
Acceptance: identical key sets, parent counts, configuration labels
required; EXACT equality required for the three crypto presets (their
tapes must not move); MNQ and MES changes PERMITTED but never
required (a fitted value may legitimately equal a declared one);
every compared measurement validated finite and positive before any
ratio. The historical `projection` and `independent` modes, their
frozen baselines, and `tick-composition-protocol-8.json` are not
edited. The mode's frozen baseline is the currently shipped
protocol-9 constants (unchanged by 5fc974d, resized last by the
protocol-8 remeasure), numerically:
`CHECKPOINT_K = 4_194_304`, `SWEEP_DRAIN_BUDGET = 1_434_000_000`,
`MAX_WARMUP_MATERIALIZATION_TICKS = 162_349_000_000`,
`fanout_depth = 1_048_576`. Gate, copy-pasteable before protocol 10
exists: `python3 analysis/tick_composition_ratios.py --selftest` - a
selftest for the new comparison logic against a synthetic fixture
pair written FOR it, pinning the ratio arithmetic, the
crypto-equality assertion both ways, the finite/positive refusal, and
wrong-version refusal (version rejection alone proves nothing about
the arithmetic). Brick R also builds the
`--verify-8-9-identity` flag Brick B0 runs - the fixture-identity
comparison excluding only `tape_protocol_version` and `pairing_id` -
and the selftest covers it both ways (identical-but-for-those-fields
passes; any other differing field refuses). This is an edit to an
existing reviewed script, not a new script.

**Brick H1 - the harness.** `analysis/mnq_fit.py`, stdlib-only, three
modes (`selftest`, `preflight`, `fit`), estimator lineage reused from
the pair harness under conformance pinning, sub-contract constants at
the top of the file. Its selftest coverage mirrors the pair harness:
grouping, session assignment against the frozen inventory, book
classification and every exclusion, every tolerance boundary
inclusively, the identifiability branch both ways, artifact binding,
CRN determinism of the inverse solves, family isolation of the probe
profiles.

**Brick H2 - conformance fixtures.** Synthetic fixtures where every
off-by-one and wrong-column reading yields a different answer (the
`asof_join.py` doctrine); the width/displacement fixtures pin
parent-weighting against row-weighting explicitly, and the boundary
fixtures pin the two-file stream contract (overlap, duplicate,
regression at the seam).

**Brick F - freeze.** The sub-contract constants and the fit artifact
schema are final; the artifact records their hash. The
`analysis/mnq-fit.json` schema, enumerated (distinct from Brick G's
per-seed summary schema):

- `binding`: {`job_id`, `file_hashes` (map filename -> sha256),
  `preflight_artifact_hash`, `subcontract_hash`,
  `harness_tree_commit`};
- `sessions`: {`inventory` (the frozen 23-label table with per-label
  status: usable / early-close-excluded / purity-excluded),
  `usable_count`};
- `preflight`: {`row_count`, `unsided_share`, `invalid_width_share`,
  per-class book counts, `valid_parent_quote_share`};
- `observed`: per target family, the empirical estimates and every
  reported distribution/diagnostic named in 4.2-4.8 (histograms as
  maps, quantiles as named fields);
- `solves`: per solved parameter, {`domain`, `coarse_grid`,
  `best_candidate`, `objective_scores` (search and final budgets),
  `termination`, `tie_break_applied`};
- `verdicts`: per target, {`status`: fitted / declared-unidentifiable
  / declared-misrepresented / stopped, `tolerance`, `measured`,
  `family`, from the family probes AND the final combined run,
  separately};
- `diagnostics`: the 4.8 shared-shape table and count-vs-volume
  curves;
- `landing_set`: the exact slot list whose provenance may flip,
  derived from `verdicts` alone.

Gate: review of this document plus the constants block, both
reviewers.

**OWNER GATE - first run.** The owner clears `analysis/mnq_fit.py` for
execution (selftest included). Nothing below starts before that.

**Brick M1 - selftest, and the H1-H2 keep gate.**
`python3 analysis/mnq_fit.py selftest`. A passing selftest is what
permits committing the harness at all; a failing one sends H1-H2 back
uncommitted.

RESULT, 2026-08-05: the owner cleared the first run; the selftest
caught TWO defects before passing at 68 checks, both fixed under
joint agreement with the discovery sequence recorded here, before any
real-data measurement. (1) Inclusive tolerance boundaries computed a
hair above the bound in binary; `within` adopted the pair-harness
SLACK 1e-12 convention. (2) The 4.75 refinement's
keep-the-subinterval-containing-the-best-point survivor rule was
MATHEMATICALLY DEFECTIVE - an endpoint incumbent can control bracket
selection without directional information, reproduced misconverging
to 3.136 on abs(x - 3.2) - and is AMENDED to classic ternary
comparison: evaluate m1 and m2, keep [a, m2] when f(m1) <= f(m2)
(the tie keeps the left), else [m1, b]; the coarse grid selects the
basin and ternary refinement makes the explicit local-unimodality
assumption within it; the returned candidate remains the best point
ever evaluated, smaller winning ties. SUPERSEDED the same day by the
owner's edit (second amendment, item 9): the survivor rule is the
INCUMBENT form again, now seeded with the coarse grid's already-paid
scores including the coarse winner. That restores neither classic
ternary nor local convergence - the 3.2 reproduction still stalls at
3.136 - and the regression was deliberately weakened to
preserves-or-improves-the-coarse-incumbent. The accepted trade: the
stall is bounded by a few percent of the initial bracket, both real
brackets are narrow relative to their gates (the displacement grid
spaces 0.0625 ticks against a 0.25-tick gate; the vol log-grid's
stall is under a percent of value against a 10% gate), so the coarse
grid does the converging, refinement is polish, and no generator walk
is re-spent on an already-scored point.

AMENDMENT, 2026-08-05, after a two-reviewer critical read of the
committed harness and BEFORE any real-data run (no preflight or fit
artifact existed): (1) identity binding now refuses a ledger-inventoried
file missing from disk (completeness was only checked disk-to-ledger);
(2) a header-only data file refuses instead of silently resetting the
seam check; (3) the ACF diagnostics compute the Pearson correlation of
exactly the accepted pairs (per-lag pair moments), not a global-mean
approximation biased by boundary resets; (4) the observed estimators now
report everything 4.2-4.7 names (per-session cadence stability, width
median/p90/MAD, top-size distributions and tails, displacement p90 and
inside-mid/at-touch/beyond-touch fractions, observed-side fixed-horizon
vol at 60 s and 300 s, `HORIZON_SECONDS` joining the sub-contract);
(5) the artifact's verdicts are per TARGET with tolerance, measured and
observed values, both stages separately, the landing set derived from
them alone, and `size_round_frac` carrying declared-unidentifiable as
its own status; (6) the size ECDF gate uses the same inclusive-SLACK
comparison as every other tolerance; (7) the wholesale cadence stop and
the volatility dependency read the combined run's cadence verdict, not
the probe alone; (8) artifacts serialize non-finite floats as strings,
never the non-standard NaN/Infinity tokens; (9) the trisection is
seeded with the coarse grid's already-paid scores including the coarse
winner, so the best-ever rule is honest and endpoint re-evaluation
(whole generator walks) is gone. The sub-contract hash moved; preflight
must run fresh, which is the binding working as designed.

AMENDMENT, 2026-08-05, after a second two-reviewer critical read,
still BEFORE any real-data run (no preflight or fit artifact exists).
Two contract changes and a set of harness fixes, all pre-measurement:

(1) The 4.9 side displacement gate is amended from pooled-target to
SIDE-VS-SIDE: the original text ("buyer and seller medians each within
0.5 tick of the pooled target") was wrong as a contract - a symmetric
static scalar always generates buyer ~ seller ~ scalar ~ pooled, so
the pooled form passes a generator whose asymmetry is simply not
represented, which is precisely what declared-misrepresented exists to
catch. Each generated side median must now sit within 0.5 tick of the
OBSERVED same-side median; a side with zero valid-quote parents has no
observed median, so its gate is explicitly vacuous-and-reported, never
a NaN comparison failing quietly. The pooled 0.25-tick gate and the
solve target are unchanged. An asymmetric-fixture regression (buyers
3 ticks off mid, sellers 1) proves the pooled gate passes while the
side gate refuses.

(2) The 4.3 all-prints size population is KEPT on re-argument
(excluding ~1% of prints to buy comparison symmetry discards more
evidence than it gains) but was stated nowhere, which was the actual
defect: the artifact now carries `size_population` with the definition
and composition counts (see 4.3).

Harness fixes from the same read: (3) a real fit run refuses on a
dirty tree - `harness_tree_commit` must name exactly the code that
ran; (4) generator walks are cached under `analysis/out/`, keyed by
the full invocation plus the harness commit (sound because of 3), so
a crashed multi-hour fit resumes instead of restarting and CRN-equal
re-evaluations are never paid twice; (5) the seam check covers
multi-row overlaps - any row of the previous file's final timestamp
recurring at the next file's head refuses, not just an exact
last-row/first-row match; (6) a fixed-horizon boundary landing exactly
on a parent's timestamp stays pending until a later parent flushes it,
so equal-timestamp parents all update the boundary's as-of mid; (7)
the volatility verdict lists the cadence metrics its family pass
actually reads, so a cadence miss inside the volatility probe is
visible in the verdict instead of failing it with all-true checks;
(8) the displacement solve's early-exit threshold is 0 - both medians
are bin centers on the shared 0.05 grid, so a nonzero threshold
implied resolution the estimator lacks; (9) CRLF input parses
identically to LF; (10) a Refusal exits with its message, not a
traceback. No sub-contract constant moved, so the sub-contract hash is
unchanged; the amended gate semantics bind through the harness tree
commit.

**Brick M2 - preflight.** `python3 analysis/mnq_fit.py preflight`.
Persists its artifact or refuses; a refusal stops the spec and is
reported as a delivery finding.

**Brick M3 - fit.** `python3 analysis/mnq_fit.py fit` writes
`analysis/mnq-fit.json` atomically before inspection, bound to input
hashes, sub-contract hash, preflight artifact and harness tree commit.
Inverse-calibration loops invoke the Brick G instrument
(`brokkr run mogwai -- gen --config ... --type summary ...`) with
scratch configs under `analysis/out/` (regenerable, not source).

**Brick V - verdicts.** Family-isolated probes then the final combined
run per 4.9, verdicts recorded in the artifact. Both reviewers read
the artifact; the landing set is fixed by the recorded verdicts alone.

**Brick L - the landing (built, not yet committed).** In ONE change:
`mnq.toml` passing values; provenance flipped to `kind = "fitted"`
with the corpus literal
`"MNQ.v.0 GLBX.MDP3 TBBO, job GLBX-20260805-HAPEWPABKG"` (this fit's
job; no other id appears anywhere in the landing) and window
`"2026-07 full month, N usable sessions"`; failed targets' rationales
updated instead (per-target estimator DEFINITIONS live in the fit
artifact and the preset comment block, NOT in provenance - the
`Fitted` schema carries only corpus, window and accepted diagnostics,
and stuffing definitions into those strings is refused); the MES
inheritance made loud (section 6); `TAPE_PROTOCOL_VERSION` 9 -> 10;
NEW preset-resolution tests in `mogwai-server` pinning the fitted MNQ
effective values and the inherited MES effective values (to write:
`fitted_mnq_effective_values_are_the_artifact_values`,
`mes_inherits_the_mnq_fit_loudly`), plus re-bless of whatever
existing `mogwai-server` preset expectations move. No tree state
carries fitted values under protocol 9. Verification on the BUILT
tree: `brokkr check --gate` green;
`brokkr test -p mogwai-data synthetic_spread_decomposition_at_protocol_seven --timeout 280`
(the ignored report test, focused, with the extended timeout its
source requires); `clean_regime_is_byte_identical`,
`a_quote_precedes_every_parent_burst` and
`the_trade_displacement_never_varies` all UNCHANGED and green - they
are XBTUSD-anchor constructions (2.3) and their passing WITHOUT
re-bless is the assertion.

**Brick B0 - the protocol-9 baseline, on the CLEAN SHIPPED tree,
BEFORE Brick L exists.** The pre-landing state is gone once L is
built, so this brick is sequenced after Brick R and before any
landing work: `brokkr run mogwai -- tick-composition --out
analysis/tick-composition-protocol-9.json`, then the identity gate
`python3 analysis/tick_composition_ratios.py --verify-8-9-identity`
(built and selftested in Brick R): exact equality with
`tick-composition-protocol-8.json` excluding only
`tape_protocol_version` and `pairing_id`, verifying 5fc974d's
byte-identity claim on the current implementation. A mismatch STOPS
everything and is its own finding. The fixture and its passing
identity result are kept independently as the current baseline.

RESULT, 2026-08-05: the fixture was measured (160 combinations,
4m36s) and the gate REFUSED on first run - on `projection`, a
version-derived producer label (`all protocol-N frames`), while every
measurement entry was exactly equal. The stop-and-review discipline
ran as designed: inspection established the field embeds the version
transitively, and the jointly accepted amendment is the CANONICAL
LABEL check - each fixture's projection must equal the producer's
deterministic label for its own version - which is stricter than
exclusion, since a projection differing beyond the version number
still refuses. After the amendment the gate PASSES: 5fc974d's
byte-identity claim holds on the current implementation, and the
protocol-9 baseline is in force.

**Brick B - composition remeasure (unconditional, on the UNCOMMITTED
Brick L tree).** `brokkr run mogwai -- tick-composition --out
analysis/tick-composition-protocol-10.json`, then
`python3 analysis/tick_composition_ratios.py --mode independent_9_10`.
If the standing acceptance policy proposes resized ceilings, the
resized constants, the protocol-10 fixture, the derivation in
`reference/performance.md` and the Brick L landing form ONE
keep/revert unit and land together; Brick L is NOT accepted until the
9-to-10 comparison passes. There is no committed intermediate state
carrying protocol 10 with unmeasured ceilings. Gate: the mode's
acceptance assertions green.

**Brick D - documentation.** `docs/presets.md`, purchase report 0.1
and 9.7 updated to the landed state; the `notes/` fit report written
from the artifact. Bundled with the code commits per the markdown
rules. Gate: `brokkr check --gate` green after the edits (gremlins
run on markdown too).

## 6. MES inheritance (decided)

MES inherits the MNQ fit deliberately - the amended boundary both
reviewers accepted. MES already borrows fitted NQ session evidence
through `preset = "MNQ"` (protocol 8 precedent); preserving crypto
anchors to avoid another inherited fit would ship a less coherent
tape. Brick L makes the borrow loud: an inventory of every effective
MES value that changes; MES expectations re-blessed beside MNQ's; the
inherited corpus strings name MNQ unmistakably so no MES corpus is
implied; `docs/presets.md` and the purchase report's "MES stays
all-declared" wording corrected to "MES borrows the MNQ fit as a
stated stopgap; the named ES/MES purchase remains the route to ending
it"; and the rationale records that the NQ/MNQ proxy FAIL proves
family resemblance is not interchangeability - this is a product
approximation, not claimed transfer validity. Structural isolation, an
MES-specific estimator, or any claim that MNQ evidence VALIDATES MES
are out of scope.

## 7. Keep/revert and stopping rule

Brick G and Brick R are independent instrument landings, kept on their
own gates regardless of the fit's outcome; Brick B0's verified
protocol-9 baseline fixture is likewise kept independently as the
current-tree record. Bricks H1-H2 are built
uncommitted and kept only on Brick M1's passing selftest, behind the
owner gate. Bricks L and B together are ONE coherent
keep/revert unit: values, provenance, MES, protocol 10, the new
preset-resolution tests, the protocol-10 composition fixture and any
ceiling resize are built on one tree, verified together, and committed
or abandoned together on the combined gates - Brick B runs before
Brick L is accepted, never after it is committed. The measurement
bricks (M1-M3, V) carry no revert obligation - artifacts and harness
stay regardless, as the record of what was measured.

Out of scope, named: the dynamic width/displacement response (14.4-A);
any `mnq06` decision contract (requires this fit's RESULTS); MBO and
queue modelling; fingerprint re-anchoring; mutation of any shared
shape constant; ES/MES evidence. The teardown stops at the preset
boundary plus the two named instruments: no engine or protocol type
changes beyond the version constant are authorized by this spec - if
the representability gate proves the generator cannot express a fitted
target, that is a recorded outcome motivating a successor spec, not
license to restructure here.
