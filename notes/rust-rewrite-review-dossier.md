# Rust rewrite review dossier

The single document the codex program-level review pass is pointed at.
Assembled at the close of phase 4a (`notes/rust-rewrite-phases.md`), before
the Python retires. A map, not a story: every claim below cites the file,
commit or landing record it comes from. Full narrative lives in the phase
landing records; this document does not repeat it, only indexes it.

**REFRESHED 2026-08-08 for the second review pass.** The first pass (session
019fe03a, `review bare --profile deep`) REFUSED the signature and named five
blockers; all five are now closed across eleven commits, `c279661` to
`32da5cd`. Every section below describes the tree AS IT STANDS after those
commits, not the state the first pass reviewed - section 12 is the diff, and
is the right place to start if you are the reviewer who refused. Three of the
five closures added surface that did not exist when the refusal was written
(a new `characterize` subcommand, a new `session-profile` pair, a ported
Markov simulation), so this is a review of new code, not a re-check of a
checklist.

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
| 4a | Reviewable-state prep | - | docs/AGENTS.md updated, dossier assembled, dissolution mapping verified |
| 4b | Blocker closure (items 1 to 4 of the 4b scope) | `c279661`..`32da5cd` | `brokkr check --gate` green at 674 passed, 0 orphaned |
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
- **`kernel::py_fsum`** (`kernel.rs:220`, added 2026-08-08) - `statistics.fmean`
  is `math.fsum(data) / n`, Shewchuk exact summation, which is NOT what
  `py_sum`'s Neumaier compensation computes. `build_fingerprint.py` uses
  builtin `sum` at line 168 and `fmean` at line 170 INSIDE THE SAME FUNCTION,
  so the two helpers must stay distinct - routing both through one is the bug.
  Note for the reviewer: the first implementation of this folded the partials
  left-to-right, which is wrong. CPython walks them from the largest down with
  an explicit half-even correction at the end, and only the embedded CPython
  reference values caught the difference; pinning against the committed
  `char_*.json` would have passed while being wrong.
- **`sqrt` over `x ** 0.5` - an APPROVED DEVIATION, not a parity convention.**
  The one place the port deliberately does not match CPython. `x ** 0.5`
  delegates the finite case to platform libm `pow`, so matching it bug-for-bug
  would make a committed artifact that `include_str!` compiles into the
  generator a function of the libm belonging to whoever regenerated it. `sqrt`
  is correctly rounded under IEEE 754 and identical on every conforming
  platform. Measured before ruling: the two differ in 1,618 of two million
  draws over the realistic domain, about one in 1,236, and `hour_vol` takes 192
  square roots across the eight pairs - so this is not a corner case. A
  discriminating test pins a value where the two differ, so that nobody later
  "restores parity". The cross-language exception is narrowed to the affected
  `session_profile.vol_hour` values; every other field must stay identical.
- **`exact::population_variance` - EXACT INTEGER ARITHMETIC, because floating
  point could not do this one.** The only place in the workspace that abandons
  floating point outright, and the reason is worth carrying: it is the rounding
  convention that took three wrong answers to get right.

  `check_cadence_feasible.py:187` calls `statistics.pvariance(gaps)` with no
  explicit `mu`. That does NOT subtract a rounded mean before squaring: it
  evaluates `(n * sum(x^2) - sum(x)^2) / n^2` as an exact rational over the
  binary64 inputs and rounds once at the end. The natural port - `py_fsum` over
  squared deviations from the rounded mean - is not a last-bit difference from
  that but an ILL-CONDITIONED algorithm: for a clustered series the true
  variance is a difference of quantities agreeing in almost every bit, so the
  rounding of each individual square dominates the answer.

  THE HISTORY IS THE LESSON. A one-ULP bound was claimed from a three-gap
  fixture. The third review pass refuted it by running
  `check_cadence_feasible.py --events 14` against
  `mogwai cadence-feasible --events 14`, which gave two ULPs with every other
  field agreeing bit for bit, and warned explicitly against restating the bound
  as two. A search then found three. And three NEARLY-EQUAL gaps turned out to
  be wrong BY A FACTOR OF THREE - 200 percent, not two steps - which showed the
  ULP framing was wrong in kind rather than degree. Three successive ceilings,
  each an artifact of the fixture it was derived from. A bound established over
  the fixtures you happen to have is not a bound.

  The closure is exactness, not a wider tolerance. Every finite binary64 is an
  integer times a power of two, so with `s` the smallest exponent in the sample
  both sums become exact integers against one shared scale, `n*Q - S^2` is an
  exact integer, and the only rounding in the computation is the final division
  by `n^2` - once, to nearest, ties to even. `crates/mogwai-lab/src/exact.rs`
  carries a minimal arbitrary-precision natural for it (add, subtract,
  schoolbook multiply, shift, divide by a single limb) rather than a bignum
  dependency for one function. The identity was verified against
  `statistics.pvariance` over 2,005 cases BEFORE the module was written, and the
  implementation is pinned by a generated 940-case sweep whose families
  deliberately include the clustered and adjacent-neighbour cases that broke the
  float version.

  ONE MORE ROUNDING BOUNDARY, found by the fourth pass, and a different failure
  from every one above. Rounding the exact quotient to 53 bits and THEN scaling
  by powers of two rounds twice whenever the result is a nonzero subnormal:
  every subnormal is an integer multiple of `2^-1074`, so its rounding position
  is pinned at that floor rather than at 53 significant bits. Five specific
  finite inputs landed one ULP below CPython. The position is now chosen as
  `max(leading - 52, -1074)` and the bit pattern assembled directly, so exactly
  one rounding happens for every output class. The first sweep could not see it:
  its 39 zero results exercise underflow TO zero, which is not the same class as
  correct rounding WITHIN the subnormal range. There is now a required
  `subnormal` family of 120 cases, and the test asserts they really are nonzero
  subnormals rather than zeros - otherwise a regenerated fixture could satisfy
  the family check while testing nothing.

  AND ONE MORE, from the sixth pass, on the same branch. That branch condition
  covers the subnormals AND the entire lowest normal binade, since
  `round_position` pins to the floor for any result leading at or below
  `2^-1022`. The assembly is correct across all of it; the `debug_assert` beside
  it was one binade too narrow, so debug builds panicked where release was
  right. A wrong bound is worse than no bound: it fails in the configuration
  meant to be stricter. The `subnormal` family could not catch it either,
  because its prose claimed to straddle the join while its filter kept only
  results below `MIN_POSITIVE`. There is now a separate `lowest-binade` family,
  aimed via the `x^2/4` identity rather than sampled, with the same
  really-in-range assertion. Sweep is 1,000 cases.

  `f64::powi` was removed from this path in the same change. Its precision is
  documented as varying by platform and Rust version, so leaving `powi(2)` in
  the `gap_cv2` division and the ACF denominator would have reintroduced a
  platform-dependent rounding one operation after the exact variance. Explicit
  multiplication is correctly rounded everywhere.

  What this buys beyond parity: `--fit-markov` no longer needs a ruling before
  it can consume `gap_cv2`. The debt was removed rather than deferred.
- **`fit::mtrand`'s `random()` and `weibullvariate`** (`mtrand.rs:161`, `:174`,
  added 2026-08-08) - both pinned against the CPython stream by prefix tests,
  needed by the Markov density simulation (see 5). `gamma` at arbitrary shape
  is still absent; the default path only calls it at shape 1, which
  `weibullvariate` covers.
- **First-wins tie-breaking on every mode** - CPython's
  `max(dict.items(), key=...)` returns the FIRST maximal key in insertion
  order; Rust's `max_by_key` returns the LAST. An insertion-ordered container is
  therefore only half the fix, and `characterize.rs` shipped exactly that half:
  `tick_counts` was an ordered `Vec` carrying a comment explaining why order was
  load-bearing, and then `modal_tick` was taken with `max_by_key`, which
  discarded it. `price_decimals_mode` was worse - a `HashMap` plus `max_by_key`,
  so its tie-break was nondeterministic between runs of the same input rather
  than merely divergent. Both now fold with a strict `>` so the earliest
  insertion wins, pinned by order-reversed tie fixtures. Found by the second
  review pass, on valid input the committed corpus happens not to contain.
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

All `#[ignore]`d, excluded from `brokkr.toml`'s complete profile because they
need corpus/archive state on disk - EXCEPT
`parity3a_cadence_feasible_verdict_matches_the_committed_cadence`, which needs
nothing but the committed `cadence.json` and now runs in the gate. That is why
the 3a pair is named in full in `brokkr.toml` rather than skipped by the
`parity3a_` prefix: the broad prefix was catching the cheap non-ignored test
along with the corpus-dependent ones, and a gate nobody runs is not a gate.

| Gate file | Reproduces | Result | Exclusions (each independently verified) |
|---|---|---|---|
| `mogwai-cli/tests/parity12a.rs` | Observed (22 sessions, 83s) + 8 generated seeds (~26s ea) | PASS | none |
| `mogwai-cli/tests/parity12a_aggregate.rs` | 2b's bootstrap/ladder sections | PASS, 2.2s | none |
| `mogwai-lab/tests/parity12a_i.rs` | Assembly from caches | PASS, ~6.5s | top-level `cost`, both live-cost classes |
| `mogwai-cli/tests/parity12a_ii.rs` (two tests) | Live `mogwai measure` run | PASS (golden gate from clean HEAD `0cf2d49`) | top-level `cost`, `binding.harness_tree_commit`, each seed's `cost` |
| `mogwai-lab/tests/parity3a.rs` (fingerprint) | `fingerprint.json` | PASS, typed-canon identical | NONE - the `allowed` list is DELETED, not updated (see 8) |
| `mogwai-lab/tests/parity3a.rs` (cadence) | `cadence.json`, live 3-archive (~230M rows) run | PASS, ~69s | `provenance.generated_utc` |
| `mogwai-lab/tests/parity3a.rs` (cadence-feasible) | the structural `PROCEED` verdict, and NOTHING MORE - it calls `verdict()` and asserts the string | PASS | see below: this row previously overstated its own coverage |
| `mogwai-cli/tests/parity3b.rs` | `mnq-fit.json`, 132/132 walk-cache hits | PASS, 82s | `binding.harness_tree_commit`, `binding.subcontract_hash`, `binding.preflight_artifact_hash` (all confirmed stale-input drift, see 8) |
| `mogwai-lab/tests/parity3b_session_profile.rs` | `fit_session_profile.py preflight`/fit, live NQ archive | PASS, 2s | none (only `session.dow_weight` is reproducible from this archive today; see 8) |

Every exclusion above was verified by direct comparison against a live
Python run at port time, never assumed honest by construction.

WHERE THIS TABLE LIED, recorded because the correction is the point. The
cadence-feasible row claimed to cover "structural verdict AND the full
3,000,000-event Markov density run". It does not: the test calls `verdict()`
and asserts `PROCEED`. The 3M field-for-field agreement was real but was
established by hand, so the density report had NO gate at all - which is how a
variance divergence sat in the tree unnoticed behind a `1e-12` tolerance in the
only test that touched it. The second review pass caught the overstatement in
the same finding that caught the divergence.

The density report is now gated properly, in `cadence_feasible.rs`'s
`gap_cv2_parity` module rather than in `parity3a.rs`: BIT-EXACT equality on
every reported field, at the default 3,000,000 events, at `--events 14`, and at
the 5,000-event fixture. No tolerance and no ULP bound anywhere - see section 2
for why the variance is computed exactly rather than approximately.

## 4. Parity-frozen defects awaiting 4b

Deliberately mirrored bugs, kept in both implementations so the parity gates
stay honest. Full text in `notes/todo.md`; summarized here for the reviewer.

- **Unguarded numeric conversions in the TBBO stream contract**
  (`analysis/mnq_fit.py`'s `parse_stream` and `crates/mogwai-lab/src/stream.rs`):
  price/size/bid_sz/ask_sz conversions carry no named refusal, so a malformed
  non-integer field crashes instead of refusing. Mirrored deliberately;
  decide whether it joins the refusal contract once the Python retires.
  Found phase 1. EXPANDED by the first review and still open: making the
  numeric parsing fallible is not sufficient, because `stream.rs` panics on a
  short row before any conversion happens. Row width must be validated first.
  This is 4b item 6 and is deliberately NOT closed yet - it is a real fix to a
  deliberately mirrored bug, so it lands with the retirement rather than
  before it, while the Python is still the parity reference.

No other parity-frozen defect entries exist in `notes/todo.md` as of this
writing; phases 2-3b added none to that class.

## 5. Documented scope gaps

- **`check_cadence_feasible.py`'s Markov density re-simulation - CLOSED
  2026-08-08.** `simulate_markov` (`cadence_feasible.rs:135`) is ported and
  wired into the default path, and `mogwai cadence-feasible` now reproduces
  `python3 analysis/check_cadence_feasible.py` field for field at the full
  3,000,000 events: mean 51.019534657973, median 3, p95 357, zero_frac
  0.129516386850407, and both gap ACFs to full precision. A 5,000-event
  fixture pins the simulation itself, so a draw-consumption, bucketing or
  state-update difference fails loudly rather than silently reproducing the
  aggregate.

  TWO CORRECTIONS this dossier previously carried, both material to the
  reviewer. First, the phase-3a framing above - "a secondary diagnostic in the
  Python itself (never gates the structural verdict)" - was WRONG. The Python
  default path exits nonzero when the realized density misses the feasibility
  bands (`check_cadence_feasible.py:275`); it is a GATE, so the unported Rust
  could exit 0 where the script exits nonzero. Second, verdict-only
  equivalence was argued as possibly sufficient for a stochastic band check
  and was REFUTED with evidence: the simulated median is 3 against a measured
  4, sitting exactly on the allowed lower boundary of measured-minus-one, so a
  different draw stream giving 2 flips the verdict. Bit-exactness here is load
  bearing, not fastidiousness.

  STILL OPEN, and tracked as capability rather than a retirement blocker:
  `--fit` and `--fit-markov`, which need `math.gamma` at arbitrary shape.
- **Fail-open decoding in `cadence_feasible.rs` - CLOSED 2026-08-08.** It had
  substituted `0.0` for missing or nonnumeric fields, so a document carrying
  `children_mean.anchor` but no `children_single_frac` returned PROCEED where
  Python raises. Strict schema refusal landed with the simulation.
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

## 8. Drift findings (both RULED 2026-08-08, see section 10; the first is now LANDED)

- **`fingerprint.json`'s `empirical_ranges.modal_tick.max` - LANDED at
  `7852e2f`.** Re-committed at `0.1`, rebuilt from all eight reports
  regenerated through the new `characterize` subcommand; the diff is exactly
  one leaf, and XBTUSD, the anchor, reports `0.1` directly. The parity gate's
  `allowed` exception list is DELETED rather than updated, and its absence is
  the evidence. No `TAPE_PROTOCOL_VERSION` bump, under a narrow exemption now
  recorded durably in `AGENTS.md`; the reviewer should read that ruling
  directly, since the bump rule is otherwise unconditional and the exemption
  explicitly does not generalize to another leaf.

  LATERAL, found while regenerating: the staleness is broader than this leaf.
  Two `n_hist` bins disagreed with the committed reports while their sums
  matched exactly. That looked like a binning defect in the port until a fresh
  Python run reproduced the Rust numbers - so it is input drift of the same
  class, not a port defect, but it means the committed `char_*.json` era is not
  cleanly recoverable. `char_*.json` is gitignored, so the regeneration
  provenance lives only in `7852e2f`'s commit message.

  The original finding, for context: committed value
  `0.25` (the exact ceiling MNQ's tick sits on); regenerating from today's
  gitignored `char_*.json` inputs - confirmed in BOTH the unmodified Python
  and the Rust port - yields `0.1`. The `char_*.json` files were regenerated
  locally after the commit that produced `fingerprint.json`, moving the
  anchor's modal tick without anyone re-running `build_fingerprint.py`. Not a
  port defect. `fingerprint.json` is compiled into the generator via
  `include_str!`, which is why this was first written up as a
  `TAPE_PROTOCOL_VERSION` decision - overstated, see section 10: the leaf is
  diagnostics-only and moves no tape byte. RULED: re-commit at `0.1`. Found
  phase 3a.
- **`mnq-fit.json`'s two binding hashes**: `binding.subcontract_hash`
  (artifact: `35e5b033...`) and `binding.preflight_artifact_hash` (artifact:
  `adf6b8e7...`, hashing a preflight file that today reads `96013588...`) are
  both stale relative to today's tree, because the protocol-12a constants
  joined `SUBCONTRACT_KEYS` after the protocol-11 fit ran. Running
  `mnq_fit.py`'s OWN `subcontract_hash()` today returns `1ca79d9c...`,
  byte-identical to what the Rust port computes now AND to what the
  committed `mnq-fit-preflight.json` already records - not a port defect,
  and cross-verified against a direct Python run.

  Filing this as the same stale-input-drift class as the fingerprint
  finding was WRONG, and section 10 rules the other way. The fingerprint's
  committed value no longer follows from its inputs; this one does. The
  protocol-11 fit never read a single protocol-12a constant, so 35e5b033 is
  an accurate record of what it ran under and the divergence from today's
  tree is expected by construction rather than stale. RULED: leave both
  hashes as committed. Found phase 3b.

## 9. Deviations from standing process

Owner-authorized for the duration of this program: no codex sparring during
the build (codex capacity reserved for the single program-level review pass
phase 4 gates on), LSP diagnostics suppression, and agents self-checking
their own landings rather than a second-agent review per slice. Recorded
here because none of it is otherwise written down, and a reviewer comparing
this program's process against the repository's normal review cadence needs
to know it was a deliberate, bounded exception rather than an oversight.

That exception ENDED with the first review pass. The five blocker closures were
each argued in a codex spar session before landing, and the implementer lost
two of four rounds: verdict-equivalence for the Markov gate (refuted on the
median sitting exactly on the band edge, see 5) and the initial version-bump
reasoning. One round went the other way - the hardcoding half of the intake
blocker was withdrawn by codex, on the ground that the Python fixes the same
pairs, archive month, anchors and preset in the same places, so the port
mirrored existing debt rather than adding it. That debt is forward work toward
the open instrument set, not a retirement blocker.

## 10. Owner decisions, RULED 2026-08-08

All four are closed. The authorized work is recorded in full in
`notes/rust-rewrite-phases.md`'s 4b scope block; the rulings and their
grounds are indexed here, including where the phase-3b assessment this
dossier carried turned out to be wrong. (These entries lived in
`notes/todo.md` until 2026-08-08, when the live arc moved out of that file
under the owner's rule that `todo.md` carries only parked work.)

- **`select_windows.py` - ABSORB, whole.** All four phases become the
  bar-frame intake station in `mogwai-lab`. Two corrections to the earlier
  assessment. First, there is no `targets-frozen.json` gate to absorb
  against: that artifact is the BTCUSDT microstructure target set, one of
  the two hash-pinned frozen INPUTS to the sampling-frame experiment, and
  this script never touches it. Its own output is
  `cme_daily_features.json`, a regenerable gitignored cache, so a gate must
  be blessed before the port can be matched against one. Second, the
  "re-sentence to KEEP until a purchase question returns" option rested on
  a closed corpus. The corpus is open: mogwai must serve whatever symbol
  gets traded next, so the purchase question returns with every instrument.
  The BTCUSDT rejection of volatility-stratified selection
  (`association-result.json`, `DATA-PURCHASE-REPORT.md` 7.2) rides along as
  a recorded prior on `select`/`plan` rather than retiring them - it is one
  observation on one crypto pair, and running the method on the next
  instrument is the only way to learn whether it generalizes.

  Sharper ground, from the preregistration's own section 7.1 (unread when
  this was ruled): the experiment validated ONLY the `rv`-rank association
  and states that it does not validate the five-feature farthest-point
  selection, whose windows "remain unvalidated by this experiment either
  way it lands." So `plan` carries a real rejection and `select` carries
  no verdict at all. Costing note: the machinery that ran that test -
  `build_bars.py`, `build_targets.py`, `spearman_association.py`,
  `run_association.py` - was deleted at `9170f45` and needs resurrecting
  from git before the method can run on a second instrument.

- **`tick_composition_ratios.py` - ABSORB, as its own subcommand.** The
  assessment recorded above ("no independent estimator, a report generator
  over Rust-produced fixtures") does not survive reading the file. It IS an
  independent estimator: the resize policy in `compare()` - worst p99.9
  ratio, two-times headroom, power-of-two or next-million rounding, then
  the larger of that and the required reach - is the decision procedure for
  four SHIPPED constants. It also carries three acceptance gates that
  refuse a protocol landing before any ratio is computed, a whole-tree
  finite-and-positive leaf validator, and a 27-check selftest pinning the
  arithmetic. `reference/performance.md` cites it by name at five sites,
  and it is the origin of the rejected protocol-11 fanout proposal that
  `the_fanout_default_carries_the_protocol_11_exception` now pins. So it
  lands as its own subcommand, NOT a `--report` mode on `tick-composition`:
  fusing them would let one command measure a fixture and bless it. The
  baseline-tables-as-data constraint from the original assessment was
  correct and is kept.

- **`fingerprint.json` - RE-COMMIT at `0.1`. LANDED `7852e2f`.** The
  `TAPE_PROTOCOL_VERSION` framing in section 8 above overstated the
  stakes: `empirical_ranges` is diagnostics-only by its own `_doc`, and
  `modal_tick.max` is read at exactly one site,
  `Scalars::empirical_diagnostics`, so no tape byte depends on it. What
  the committed `0.25` actually buys is a false negative - it is the exact
  inclusive ceiling MNQ's tick sits on, so the MNQ preset clears the
  corpus-range check on a stale input rather than on evidence. The
  regenerated `0.1` makes the diagnostic fire honestly and MNQ accepts it
  in provenance, as it already does for three other fields.

- **`mnq-fit.json`'s binding hashes - LEAVE AS COMMITTED.** Filing this
  alongside the fingerprint drift as the same class was wrong. The
  fingerprint's committed value no longer follows from its inputs; this
  one does. `subcontract_hash` 35e5b033 accurately records the constant
  set the protocol-11 fit ran under, and the protocol-12a block joined
  `SUBCONTRACT_KEYS` afterwards without that fit ever reading it - which
  phase 3b itself demonstrated by reproducing every fitted number at
  132/132 cache hits while only the binding differed. Re-committing to
  1ca79d9c would assert a binding that never happened, the precise claim
  `mnq_fit.py`'s tamper selftest exists to refuse. The genuine finding is
  narrower and stays recorded: `mnq-fit-preflight.json` already carries
  1ca79d9c, so the artifact is readable but not extensible until a fresh
  fit runs, and the flat single-namespace subcontract hash is a design
  defect the port should fix by scoping the hash per mode.

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
  no counterpart, none expected. The REASON stated here was wrong and is
  corrected 2026-08-08: `probe_binance_klines.py` is not DEAD. This very
  test imports it (`test_characterize.py:34`), which is why the `9170f45`
  retirement kept it while deleting the rest of the triage's DEAD list.
  The conclusion is unchanged - it was never in the ABSORB set - but it
  survives as a live test dependency, not as a spent one-shot.
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

## 12. What changed since the refusal, 2026-08-08

For the reviewer who refused: this is the whole diff between the tree you saw
and this one. Eleven commits, `c279661` to `32da5cd`. Each blocker's detail is
in the section named beside it; none of them is summarized here twice.

| Blocker as you raised it | Closed by | Detail in |
|---|---|---|
| No `mogwai characterize` subcommand - retiring `characterize.py` severs `char_*.json` production in both languages | `792ce08`. Covers both Python entry points, the per-corpus case and `run_corpus.py`'s multi-pair fan-out | phases doc 4b item 1 |
| No CLI surface for the session-profile fit | `9399750`. `session-profile preflight` and `fit`; preflight reproduces the Python field for field at 5,891,412 rows. Both lost their hardcoded MNQ preset to a `--preset` argument | phases doc 4b item 1b |
| `cadence-feasible` drops the Markov density GATE and can pass open; `0.0` substitution returns PROCEED where Python raises | `bd0e01e` (strict schema refusal), `f07f74d` (the simulation) | section 5 |
| Fingerprint loader: `sqrt`/`fmean` silent float divergence, glob-order dependence, fail-open `pair`/`n_trades` | `8fb8d69`, `1d341b0` | section 2 (`py_fsum`, `sqrt`) |
| `brokkr check --gate` red: lateness failure plus an orphaned parity gate | `8fb8d69` (un-orphaned), `32da5cd` (lateness quarantined). Green at 674 passed, 0 orphaned | below |
| The fingerprint drift you were shown as pending | `7852e2f`, plus `7adc008` correcting the durable protocol history | section 8 |

Two of those want your judgement specifically, because they are arguments
rather than ports:

- **The lateness quarantine (`32da5cd`)** is not a fix and not a relaxation.
  `tape_lateness_under_acceleration` is excluded from the DEBUG lane only, on
  the argument that a debug lane cannot validly judge a release wall-clock
  contract - so its red was not evidence about the property, and running it
  there was itself a changed measuring instrument. The 50 ms assertion is
  untouched and the test stays directly runnable. The exclusion claims nothing
  about whether this host meets the budget, and the honest state is that the
  environment sensitivity is WORSE than previously recorded: a release rerun
  failed at 311 ms with a load average of 1.46 across 32 visible CPUs, which
  rules out the load-average precheck the old note proposed. Open in
  `notes/todo.md`, deliberately not closed.
- **The `TAPE_PROTOCOL_VERSION` exemption** for the fingerprint re-commit, now
  in `AGENTS.md`. The rule is otherwise unconditional and nothing can detect a
  missed bump, so an exemption is a real cost; the grounds are one leaf, an
  exhaustive reader audit finding `Scalars::empirical_diagnostics` as its sole
  consumer, and version 12 already reserved for 12b.

### The second pass, 2026-08-08

Session 019fe13a, 17m25s, pointed at this packet. REFUSED AGAIN, on four
findings, and accepted both judgement calls above - the lateness quarantine as
"a correct debug-versus-release measurement boundary" whose release property
stays explicitly uncertified (its own rerun failed at 360.0 ms p99 under load
average 1.36, independently confirming the sensitivity is not load-driven), and
the version exemption, verified the strong way by running a fresh eight-pair
characterization and synthesis and getting a value identical to the committed
fingerprint.

The four findings shared one shape, which is the durable lesson: each closure
held on the committed corpus and failed one layer below where its gate looked.
That is the same ground the first refusal stood on. Closures were verified
against the artifacts rather than against the contract.

| Finding | Where it failed | Closed by |
|---|---|---|
| `characterize` breaks the path-shaped input it advertises: the output name came from the raw CLI argument, so `characterize path/to/KEUR.csv` wrote `char_path/to/KEUR.csv.json`, invisible to `load_reports` | the subcommand existed, so the blocker read as closed; the write path had no test | name derived from `report["pair"]`, matching `characterize.py:247`/`:487`; three CLI tests driving the real binary, including one asserting the loader can see the result |
| Both modes tie-break the wrong way, one of them nondeterministically | valid input; the eight committed reports contain no tie | first-wins folds, order-reversed fixtures (see section 2) |
| `level_verdict`/`level_queue` still fail open on `single_print_frac`, `vol_dispersion`, `size_dispersion` | the loader was made strict, the scorer was not | `level_field` refuses per field; each field dropped individually in the test, plus a case pinning that the fail-open direction was toward a manufactured PASS |
| `gap_cv2` diverges from `statistics.pvariance`, unstated, hidden by a `1e-12` band | the only test touching the density report used a tolerance | tolerated on reachability, bit-exact pins on every other field - but the one-ULP bound asserted here was WRONG and the third pass refuted it; see the third-pass record below and section 2 |

Consensus on the fourth was reached by argument rather than by capitulation, and
both sides moved. The reviewer accepted that reachability governs whether a
deviation is APPROVABLE, and held that the absence of a STATED deviation blocks
regardless - so a silent difference behind a broad tolerance blocks, while the
same difference, stated and bounded, does not. It also corrected the
implementer's premise: CPython does not round the deviation before squaring, so
the two-double expansion proposed as a cheap exact port would not have been
parity at all.

Gate after the four closures: 682 passed, 0 orphaned, up from 674.

### The third pass, 2026-08-08

Session 019fe13a again, 6m05s. REFUSED. It re-ran the focused tests for all of
the second pass's closures - characterize naming and loader visibility, both tie
modes, all three level-field refusals, the false-PROCEED direction, the variance
discriminator and the 5,000-event simulation - and found no further defect in
closures 1 through 3. It also ruled the TBBO short-row panic explicitly NOT
grounds for refusal, since it mirrors the Python today and item 6 is correctly
ordered before script removal, while stating that an eventual signature stays
conditional on fixing both the width check and the panicking numeric conversion
before item 7.

It refused on two things, and named the first at the level of the pattern
because it had been asked to.

RESOLVED after the pass: the reviewer ruled BUILD THE EXACT ACCUMULATOR rather
than widen the approval, on the ground that reachability shows the value cannot
change the exit status but does not make a printed number sound - CLI output has
consumers outside the in-tree call graph, and determinism is not correctness. It
also refused the alternative of suppressing the field, since that would remove
Python capability. Both are right, and the debt is now gone rather than
deferred: see section 2's `exact::population_variance` entry. It further found
that the platform-independence claim made for the float version was not even
true as implemented, because `f64::powi` sat in the same expression and its
precision varies by platform and Rust version.

1. **The one-ULP approval was false for valid CLI input.** Established only over
   a hand-picked three-gap vector and the 5,000-event artifact, then asserted as
   a universal envelope. `--events 14` is valid input on both sides and gives
   two ULPs. This is the same shape as every prior finding: TRUE ON THE SELECTED
   FIXTURES, FALSE OVER THE ACCEPTED CONTRACT, which is exactly what
   `reference/architecture.md`'s parity contract forbids. It warned specifically
   against re-stating the bound as two, since that would repeat the mistake -
   and it was right to: a search then found three, and a nearly-equal-gap case
   is wrong by a factor of three. See section 2; the deviation is now recorded as
   unbounded, with no ceiling claimed.

2. **The claimed green gate did not reproduce.** `brokkr check --gate` stopped
   at a `clippy::semicolon_if_nothing_returned` error in `characterize.rs`. The
   cause was ordering: the gate was run, and THEN `brokkr fmt`, which wrapped a
   match arm into a block and introduced the lint. The 682-pass claim described
   a tree that no longer existed by the time it was reported. It also noted the
   claim sat on an uncommitted working tree, so it was not attached to a
   reviewable commit at all.

Both are closed. The lint is fixed, the gate re-run AFTER formatting - 684
passed, 0 orphaned - and the false bound is replaced rather than renumbered:
`gap_pvariance` now states the deviation is unbounded, pins our own values as
regression pins rather than parity claims, and carries three cases including the
factor-of-three one. A test written during that work asserted a large ULP
distance against a hand-rolled "exact" reference that was itself a naive sum,
1.5e14 ULPs from both real values - it passed while measuring nothing, and was
replaced with CPython's bit-pinned value. Worth recording as the same disease in
miniature.

STILL OPEN in 4b, and none of it blocks the signature by the scope as ruled:
the `select_windows.py` absorption (item 2), the `tick_composition_ratios.py`
absorption (item 3), per-mode subcontract hash scoping (item 5), the TBBO
short-row fix (item 6, section 4), and then the retirement itself (item 7).
`--fit`/`--fit-markov` on `cadence-feasible` remain absent as capability.

The question for this pass is NOT whether these six rows are closed - the tree
answers that and you can re-run the gates. It is whether the tree is now
retirement-ready on the ground your refusal actually stood on: what the gates
do not cover. Three of the closures are new surface that did not exist when you
wrote that, so findings outside the six rows above are expected rather than out
of scope.

That question was asked and answered; the second pass is recorded above.
