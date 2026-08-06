# The MNQ generator successor: floor-aware children, local size tail,
# minute-range gates, protocol 10

Implementation specification, 2026-08-05. Written against
`reference/technical-implementation-spec.md` and spawned from the fit
verdict of `notes/mnq-tbbo-fit-spec.md` (Bricks M3/V RESULT) and its
artifact `analysis/mnq-fit.json` - the TODO source this spec exists to
resolve. Scope, mechanisms and gates were argued to consensus by both
reviewers across the fit's joint read, the owner's chart review, and
two diagnosis rounds (print-level dissection of the 420.75-point
minute; source-level verification of the volatility rails).

This is a `notes/`-class document: transient, no truth guarantee,
nothing durable may cite it.

---

## 1. The goal

Make the generator able to REPRESENT July MNQ within the frozen fit
tolerances where the evidence shows it currently cannot, gate what it
still cannot represent honestly, then rerun the frozen fit and land
protocol 10 as ONE final history unit. Four items, each motivated by
committed evidence:

1. **Floor-aware child conditioning** - the confirmed cadence
   mechanism (probe values predicted to 0.02%).
2. **A target-local size sigma** - the measured p99 miss (generated
   14 vs observed 8 at shared `SIZE_LOG_SIGMA = 1.15`).
3. **Minute-range representability gates plus a state-level forensic
   probe** - the owner's 420.75-point bar, diagnosed as a V-shaped
   volatility-cluster tail, mechanism to be established by trace, not
   assumed.
4. **The reopen-gap limitation recorded** in durable documentation -
   not implemented; July TBBO does not identify a jump distribution.

The crypto presets stay BYTE-IDENTICAL through every change; the
hourly session-profile granularity (the 13:30 cash open smeared
across the 13:00 UTC bucket) is explicitly out of scope this round.

## 2. Survey of the ground

### 2.1 The cadence mechanism (confirmed)

`consts.rs` derives `ARRIVAL_QUIET_CHILDREN_MULT = 0.20` and
`ARRIVAL_ACTIVE_CHILDREN_MULT ~= 1.430769` under the identity
`0.35 * quiet + 0.65 * active = 1`, which assumes the scaled means
survive unchanged. `SweepShape::new` (`dynamics.rs`) clamps an
impossible sub-one mean to `1 + epsilon`, breaking the identity;
`source.rs` still applies the original residual-single calculation,
whose impossible quiet contribution clamps the active residual to
zero, collapsing the active branch to pure geometric. At configured
`children_mean = 1.171113`:

    quiet intended  1.171113 * 0.20 = 0.234  -> clamped to 1
    active          1.171113 * 1.430769 = 1.6756
    realized mean   0.35 * 1 + 0.65 * 1.6756 = 1.43914  (probe 1.43937)
    single frac     -> 0.73792                          (probe 0.73779)

July MNQ observed: `children_mean 1.1711`, `children_single_frac
0.9049`, 29,605,800 parents. The crypto anchor (`children_mean 8.49`)
never approaches the floor.

### 2.2 The size tail (measured)

`SIZE_LOG_SIGMA = 1.15` is module-level shape. The solved MNQ latent
median 0.9556 under that sigma generates p99 = 14 contracts against
the observed 8; the ECDF and mean gates passed. 34.7M July prints
identify the MNQ tail; no other instrument's evidence is implicated.

### 2.3 The volatility architecture (verified against source)

The purchase report's section 2.2 constants are STALE. Shipped
(`consts.rs`): `GARCH_ARCH = 0.02`, `GARCH_GARCH = 0.979`
(persistence 0.999, half-life ~693 events), `STUDENT_T_DF = 4`,
`GARCH_SIGMA_CAP = 1e-3`, `FEEDBACK_RETURN_CEILING = 4e-3`,
`REALIZED_RETURN_CEILING = 5e-2`. Order per event (`source.rs`): draw
t(4); update GARCH with sigma capped; base return clamped at the
feedback ceiling; multiplied by `session.vol_mult * regime.vol_mult`
(outside feedback, inside realized); realized clamp; move the mid. At
price 28,284 the one-event envelopes are ~28.3 points (sigma cap at a
unit innovation), ~113 points (feedback), ~1,450 points (realized).
The crypto generator anchor is `VOL_SCALAR = 1.2e-5`; the fitted MNQ
value 6.96e-6 is 0.58x that anchor.

The 420.75-point minute (2026-07-07 15:45 UTC, chart seed 42,
config `analysis/out/mnq-fitted-candidates.toml`): a V (28008.5 ->
27716.25 -> 28137.0) across the full 60 s; steps balanced 839
up / 817 down, largest monotone sub-run 63 points, magnitudes
commonly 1.5-2.25 points; FEWER prints than every neighbor minute
(3,218 vs 3,674-4,792); neighbor ranges 10.5-44.75 points; the
minute before ran warm. Print magnitudes sit far below every rail,
so the working hypothesis is an ARCH-amplified t(4) episode - an
unconstrained volatility-cluster tail - and the trace is expected to
show ZERO clamp hits. The normal-minute body (11-17 points) matches
the owner's real-MNQ texture; only the tail regime is wrong.

### 2.4 What depends on the changed code

- `SweepShape` consumers: `begin_event`/child drawing in `source.rs`;
  `FlowSurge.children_mult` is constrained to [1, 100], so the crypto
  anchor at 8.49 NEVER enters the floor branch even under surge -
  which is what makes high-mean byte identity achievable.
- `SIZE_LOG_SIGMA` consumers: the size distribution construction and
  `integral_lot` derivation (reads the median, not sigma), scalar
  validation, the harness's inverse solve (holds sigma fixed - it
  becomes a per-candidate input).
- Goldens: `clean_regime_is_byte_identical` (XBTUSD anchor,
  children_mean 8.49, sigma 1.15 default) MUST remain byte-identical,
  as must the fill goldens and the three seam tests; the three crypto
  presets must remain byte-identical INCLUDING armed-FlowSurge cases,
  pinned by new tests. `independent_9_10`'s crypto-equality
  acceptance survives as designed.
- The fit harness: solves and probes gain sigma as a solved
  parameter; the 4.9 table gains the minute-range gates; the
  sub-contract hash MOVES (new constants), so preflight reruns.
- `TAPE_PROTOCOL_VERSION`: 9 -> 10, in the SAME final history unit as
  the fitted preset (section 6).

## 3. The changes

### 3.1 Floor-aware child conditioning (Rust)

The branch is selected ONCE per instrument, on the BASE configured
mean - never on the surge-effective mean - so no runtime path ever
crosses between the two arithmetics and no discontinuity is
reachable:

- `children_mean * ARRIVAL_QUIET_CHILDREN_MULT > 1` (base mean above
  5): the LEGACY branch - the current arithmetic byte-for-byte, at
  every `children_mult`. The crypto presets (anchor 8.49) and the
  golden live here always, surged or not.
- Otherwise the FLOOR-AWARE branch, at every `children_mult`,
  parameterized per effective mean:

      effective_mean = children_mean * children_mult
      quiet_eff      = effective_mean * QUIET_MULT       (0.20)
      quiet_mean     = max(1, quiet_eff)
      active_mean    = (effective_mean - QUIET_SHARE * quiet_mean)
                       / ACTIVE_SHARE
      active_single  = (children_single_frac
                        - QUIET_SHARE * quiet_single) / ACTIVE_SHARE
      where quiet_single, the quiet branch's single-child
      probability, is EXACTLY max(children_single_frac, 1/quiet_eff)
      when quiet_eff > 1, and exactly 1 when quiet_eff <= 1.

  At `children_mult = 1` and July's targets this yields active mean
  1.26325 and active single 0.85369 (feasibility floor 1/1.26325 =
  0.79161 - the existing one-plus-geometric mixture expresses it).
  At `children_mult = 2` the unconditional mean is 2.3422 as
  requested, active mean ~3.065 - the surge scales the TARGET, and
  the solve preserves it exactly, unlike the broken identity.
  Validation refuses an infeasible active solve (active_single below
  the feasibility floor 1/active_mean, or above 1) with a named
  error AT CONSTRUCTION, evaluated at children_mult = 1. There is NO
  runtime surge-arming refusal: `TickSource::arm_flow_surge` returns
  unit and the live control path cannot propagate one without
  changing the data trait, which section 5 excludes. Instead a named
  test (`floor_branch_feasibility_is_monotone_under_surge`) proves
  that a configuration feasible at children_mult = 1 remains
  feasible for every multiplier in [1, 100] - construction-time
  validation therefore covers the whole surge range.
- Tests cover the base-mean branch boundary (an instrument at
  exactly 5.0), `children_mult` at 1, at 100, and at the value where
  quiet_eff crosses 1 within the floor-aware branch (continuity of
  the unconditional targets across that internal transition is
  asserted numerically).

### 3.2 The target-local size sigma (Rust)

`size_log_sigma` joins `GeneratorScalars`, defaulting to exactly
1.15 - the shared value becomes the default, not a refit. Crypto
presets omit it and stay byte-identical; MNQ receives an
inverse-fitted value; MES inherits loudly per the standing decision.
Validation bounds it to [0.1, 3.0]. Brick S's survey obligation:
inventory EVERY `SIZE_LOG_SIGMA` consumer before the change - the
constant definition, the lognormal construction in `source.rs`,
every appearance in `fingerprint.rs` (validation, diagnostics,
derived reporting), and every test naming it - and route each
through the scalar; the implementation commit lists the inventory.

The joint solve, pinned exactly: for each of the 16 FIXED sigma grid
values (linear, [0.4, 2.0], so steps of ~0.107), run the COMPLETE
existing median solve (64-point coarse grid plus trisection, all
existing termination and tie rules, prewarmed); compare the 16
converged (sigma, median, score) triples lexicographically on score;
NO sigma refinement between grid neighbors - the winning grid sigma
IS the fitted value, a deliberately grid-resolution answer whose
adequacy the representability gates judge. Ties break toward the
smaller sigma, then the smaller median. The artifact records all 16
per-sigma solve records plus the winner; evaluation budget is 16
complete median solves (prewarm the 16 x 64 candidate matrix). The
sigma grid constants join `SUBCONTRACT_KEYS`.

### 3.3 Minute-range gates and the forensic probe (harness + Rust)

Summary mode gains per-minute range accumulation over open minutes:
`minute_range_ticks_hist` keyed in INTEGER TICKS (a whole-point bin would
discard the native 0.25 resolution and move tail quantiles), plus
per-seed `minute_range_max_ticks` and `minute_range_second_max_ticks`
(unit-bearing names, amended in review to match the implementation). The per-seed gates
require per-seed data, so the probe path RETAINS the raw per-seed
summaries alongside the pooled view: `summaries_for` returns a
`SeedSummaries` pair - `{"pooled": <the pooled dict exactly as
today>, "per_seed": [<one raw summary dict per seed, in seed
order>]}` - with the SOLVER paths consuming only `pooled` (unchanged
semantics) and the probe/judge paths consuming both: `pooled` for
every existing gate, `per_seed` for the minute-range gates alone.

The observed July per-minute tick ranges are computed in the same
estimator pass, and the gates come ENTIRELY from preregistered
session-block resampling - no invented percentages:

    RESAMPLE_SEED         = 1
    RESAMPLE_REPLICATES   = 1000
    per replicate: draw 22 sessions WITH replacement from the usable
    sessions (matching the generated exposure of one seed month),
    concatenate their minute ranges, record nearest-rank p99, p99.9,
    p99.99 and the maximum
    envelope: the one-sided UPPER bound at the 99th percentile of
    each statistic across replicates (a per-statistic bound, not a
    simultaneous band - accepted policy, stated here). p99.99 is
    computed and reported as a DIAGNOSTIC only - at ~30k open
    minutes per month it rests on the top three observations and is
    too unstable to gate.

The 4.9 additions, all one-sided upper gates in the volatility
family, judged PER SEED against the envelope (never one pooled
maximum against one observed month - an eightfold exposure
asymmetry):

| target | gate |
|---|---|
| minute-range p99, per seed | <= envelope p99 |
| minute-range p99.9, per seed | <= envelope p99.9 |
| per-seed monthly max | <= envelope max |

All resampling constants join `SUBCONTRACT_KEYS`. These gates fail
the volatility family, not the landing; the stopping rule applies
target-locally as everywhere else.

`gen --type trace` is the forensic probe, pinned as a buildable
interface. A `TraceRecord` struct in `gen.rs`:

    parent_ts: u64            child_count: u32
    innovation_raw: f64       innovation_std: f64
    sigma2_candidate: f64     sigma2_realized: f64
    sigma_cap_hit: bool       garch_scale: f64
    base_return_unclipped: f64  base_return: f64
    feedback_clamp_hit: bool
    session_vol_mult: f64     regime_vol_mult: f64
    pre_realized_return: f64  realized_return: f64
    realized_clamp_hit: bool
    mid_before: f64           mid_after: f64

The values come from the REAL `GarchVol::step` result and the real
event path - never a reimplementation, and NO additional draws. The
cross-crate hook, pinned: `mogwai-data` gains a `pub struct
VolTrace` carrying the per-event intermediates (the field list
above, minus the gen-side timestamp/child bookkeeping) and two
methods on `GeneratedSource` - `enable_vol_trace(&mut self)` and
`take_vol_trace(&mut self) -> Option<VolTrace>` - populated inside
the existing event step ONLY when enabled, observation-only, no
draws, `TickSource` untouched. `gen.rs` enables it, takes one record
per parent, joins the timestamp and child count, and serializes.
CLI: `--type trace --trace-from <ns> --trace-until <ns>` (both
required with trace, refused otherwise, and validated as
`start <= trace_from < trace_until <= start + length`); the walk
runs from `--start` as normal and records only parents inside the
window; output is one JSON object per line in parent order to
`--out`. Gate test:
`trace_consumes_no_draws_and_leaves_the_tape_byte_identical` (same
seed with and without trace, tape equality). The first assignment,
copy-pasteable, on the final protocol-10 tree:

    brokkr run --release mogwai -- gen --config
      analysis/out/mnq-fitted-candidates.toml --type trace
      --seed 42 --start 1783288800000000000 --length 150420s
      --trace-from 1783438980000000000 --trace-until
      1783439220000000000 --out analysis/out/mnq-trace-1545.json

(the 15:43-15:47 UTC window around the 420.75-point minute; the
config regenerated from the successor artifact's candidates -
REGENERATE IT FIRST, the on-disk copy predates the joint solve). The
trace artifact is REGENERABLE and not committed; its findings are
recorded in the RESULT sections of this spec, which ARE the
"successor fit report" the evidence commit names - no separate
document exists. Amended scope of the claim, agreed in review:
floor-aware conditioning changes the random path, so the fixed
seed-42 window may no longer contain the former 420.75-point
excursion. The trace runs because this spec requires it and records
exactly what it observes; it explains the NEW per-seed maxima only
if an excursion is actually present in the window, and locating and
tracing the new worst minutes belongs to the future tail-shape spec. Expected per the source-level
diagnosis: zero clamp hits on all three rails, establishing the
excursion as an unconstrained t(4)/GARCH volatility-cluster tail.
That verdict decides whether any volatility-mechanism item enters a
FUTURE spec. NO volatility mechanism changes in this one: if the
minute-range gates fail after the refit, volatility lands
declared-misrepresented and the trace output is the successor
evidence.

### 3.4 Documentation (bundled with the code commits)

The reopen-gap limitation - the generated tape freezes through every
closure and reopens exactly where it closed; no reopening jump is
synthesized, and July TBBO cannot identify one - lands in
`reference/architecture.md` (the chosen durable target), the MNQ
preset's provenance prose, and `docs/presets.md`. The purchase
report's stale section 2.2 volatility constants are corrected. The
hourly-profile smear is recorded as a known granularity limit in
`reference/architecture.md` beside the session-profile description.
Gate: `brokkr check` (gremlins cover markdown), bundled with the
code commits per the markdown rules.

## 4. Order of work, bricks, gates

**Brick O - the protocol-9 oracle, FIRST, on the clean protocol-9
tree.** Tests written after a change cannot prove protocol-9 identity
without something frozen - and `gen`'s trade CSV omits quotes while
its CLI cannot arm a FlowSurge, so the oracle is an OBSERVATIONAL
test instrument, not a CSV hash. A `#[ignore]`d `mogwai-server` test,
`protocol9_tape_oracle`, walks `GeneratedSource` directly through
the resolved preset profiles over the named matrix - the three
crypto presets, seeds 42 and 7, 6-hour windows from start 0, plus
BTCUSDT with a FlowSurge armed through `TickSource::arm_flow_surge`
(start 1h, 30m, rate 2.0, children 1.5) - serializing EVERY field of
every `TickEvent` (trades AND quotes) canonically (named separator
lines with Display-formatted fields - a stable contract, amended in
review from the Debug form, whose derived layout a field rename could
silently re-key) and hashing the stream with
FNV-1a 64, named in the fixture: the workspace carries no crypto
hash crate, and an oracle detects regressions, not adversaries. Write-once semantics, pinned so a missing fixture can never
re-bless later-protocol output: when the fixture is MISSING the test
writes it only if `TAPE_PROTOCOL_VERSION == 9`, and REFUSES BY NAME
under any other protocol; when the fixture is present it always
asserts equality. It observes without changing generation, so it
lands under protocol 9. Exact command, also the Brick C/S regression
command:

    brokkr test -p mogwai-server protocol9_tape_oracle

ONE oracle covers both successor changes simultaneously. Gate: the
fixture exists with all SEVEN matrix entries (three presets times
two seeds, plus the armed-surge case), `brokkr check` green.

**Brick C - conditioning (Rust).** The 3.1 branch, plus named tests:
`the_floor_branch_reproduces_a_near_single_child_tape` (configured
1.1711/0.9049 generates within the fit tolerances on a short walk),
`the_high_mean_branch_is_byte_identical` (crypto anchor walks with
and without the change, tape equality),
`a_surge_crossing_the_floor_boundary_follows_the_branch`,
`an_infeasible_active_solve_refuses_by_name`, plus byte-identity
tests for all three crypto presets including an armed FlowSurge.
Gate: `brokkr check` green; `clean_regime_is_byte_identical`
UNCHANGED.

**Brick S - size sigma (Rust).** The 3.2 scalar, validation, and
`size_log_sigma_reaches_the_draw` /
`the_default_sigma_is_byte_identical` tests. Gate: `brokkr check`.

**Brick T - trace mode + minute ranges (Rust).** The 3.3 summary
fields and `gen --type trace`, with
`trace_consumes_no_draws_and_leaves_the_tape_byte_identical` and
`minute_ranges_match_an_independent_bar_pass`. Gate: `brokkr check`.

**Brick H - harness extension.** The joint (median, sigma) solve, the
minute-range observed pass and resampled thresholds (constants join
the sub-contract block; its hash MOVES - preflight must rerun), the
new 4.9 rows, selftest coverage for the new solve dimension, the
envelope gate per seed, and the resampling determinism. Gate:
`python3 analysis/mnq_fit.py selftest`.

**Brick F2 - freeze.** Both reviewers sign the amended sub-contract.

**The calibration loop (the fold workflow, corrected for the
composition circularity).** All on master, no branches, nothing
pushed until settled; every iteration is one commit built by
`git reset --soft <protocol-9 parent>` and recommitting, so history
never carries an intermediate:

1. Build the CANDIDATE protocol-10 code commit from the protocol-9
   parent: bricks O/C/S/T/H, `TAPE_PROTOCOL_VERSION = 10`, the
   fitted preset values from the best available artifact (first
   iteration: the protocol-9 fit's candidates), provenance, MES
   inheritance, and the fit spec's Brick L preset-resolution tests.
2. On that clean commit: `python3 analysis/mnq_fit.py preflight`
   then `python3 analysis/mnq_fit.py fit` (the sub-contract hash
   moved, so preflight is fresh by construction).
3. `brokkr run mogwai -- tick-composition --out
   analysis/tick-composition-protocol-10.json` then
   `python3 analysis/tick_composition_ratios.py --mode
   independent_9_10`.
4. If the fit's verdicts change the preset values, or composition
   acceptance proposes resized source constants, the iteration
   EVACUATES its evidence before rebuilding - the fit and
   composition modify TRACKED files (`analysis/mnq-fit.json`, the
   composition fixture), and a soft reset would strand those changes
   in the worktree, failing the next run's clean-tree binding:
   copy the iteration's evidence into ignored `analysis/out/` for
   reconstruction; `git checkout` the tracked evidence paths back to
   the candidate commit's content; `git reset --soft` to the
   protocol-9 parent; stage ONLY the intended code, preset, test and
   documentation paths; recommit; verify `git status --porcelain`
   is empty; then repeat from step 2 on the replacement commit.
5. The loop ends when a commit's own fit and composition read clean
   against the tree they ran on, with no source change proposed.
6. The EVIDENCE lands as a child commit: the fit artifact (whose
   `harness_tree_commit` names the settled code commit - a commit
   cannot contain an artifact naming its own hash), the protocol-10
   composition fixture, the ratio results, and the fit report. Gate:
   `brokkr check --gate` on the settled code commit, plus every
   brick gate below, plus the artifact's landing_set being what the
   preset carries - exactly.
7. Reversion of the whole unit is `git reset` past both commits. The
   walk cache keys on each iteration's commit, which binds evidence
   to the exact tree - and therefore a REPLACEMENT commit runs cold
   by construction; only a resumed fit on the same commit re-pays
   nothing. (This line originally claimed the opposite; corrected in
   review during iteration 3.)

RESULT, 2026-08-05, iteration 1 of the loop (candidate f4a6cb1,
fit under the moved sub-contract, ~75 minutes cold-cache): the
landing_set is the EIGHT cadence, quote and anchor targets -
floor-aware conditioning fixed cadence representability outright,
the probe reproducing 1.1711127/0.9048984 to five decimals where
protocol 9 could not express the pair at all. Three targets read
declared-misrepresented. Size: the joint solve's best candidate
(sigma 0.9333, median 1.097264) improves generated p99 from 14 to
10 contracts against observed 8 with bound 9.6 - one contract over
one quantile, ecdf 0.0057, mean and p90 clean. Volatility:
vol_scalar 8.701e-6 lands mid_rms 1.26e-5 vs observed 1.18e-5
inside tolerance, but the per-seed minute-range envelope gates fail
around it - per-seed p99.9 464-639 ticks vs observed 399, per-seed
max up to 4333 ticks vs the real month's 968. That quantifies the
chart-visible tail phenomenology: the t(4)/GARCH cluster-tail SHAPE
overproduces extreme minutes and no scalar lands a shape. Comparison
evidence from the same cache: the old 6.96e-6 scalar fails mid_rms
at 12.6 percent low while leaving the max gate almost identically
broken (7 of 8 seeds, up to 3464), so depressing the central scale
is not a tail fix and the solver's candidate is carried. Iteration 2
folds the three best candidates into the preset under declared
provenance per the Brick L post-measurement resolution in the fit
spec. The volatility envelope failure is the recorded outcome that
motivates the future tail-shape spec (reopen-gap phenomenology plus
cluster-tail shape), with the 3.3 trace as its first evidence.

RESULT, 2026-08-06, iterations 2 and 3, loop CONVERGED on 3a48f32.
Iteration 2 (candidate 4719c15, the fitted preset carrying the
declared best candidates): verdicts and fitted_candidates reproduced
iteration 1 byte-for-byte - values converged - but the composition
gate proposed resized ceilings, since the fitted cadence roughly
doubles peak tape density. Iteration 3 folded the four resizes
(CHECKPOINT_K 16,777,216; SWEEP_DRAIN_BUDGET 5,799,000,000; warmup
reach 667,299,000,000; fanout_depth 4,194,304; derivation in
reference/performance.md) and its fit reproduced the verdicts and
candidates byte-for-byte again, the artifact binding 3a48f32. The
composition remeasured ON the settled commit is measurement-identical
to iteration 2's and the proposal now equals exactly what the source
carries - convergence per Brick B. Workflow binding for the fixture,
which carries no commit field of its own: produced on 3a48f32 by
`brokkr run --release mogwai -- tick-composition --out
analysis/tick-composition-protocol-10.json`, pairing_id
000000000000000018c90efbf402c2d7-0009e35d, sha256
01aacbff8ef8a41014452957493735c8f5ea09e48adcf93ce0689a8af873ffd2,
gated green by `tick_composition_ratios.py --mode independent_9_10`
(acceptance assertions passed; ratios 1.60 / 2.02 / 2.055 / 1.84;
required reach below every resized ceiling). The evidence lands as
the child commit of 3a48f32; the iteration-1 and iteration-2
artifacts are evacuated to `analysis/out/` as
mnq-fit-iteration1-f4a6cb1.json and mnq-fit-iteration2-4719c15.json
for reconstruction.

RESULT, 2026-08-06, the frozen trace assignment, run on the settled
tree 3a48f32 from the regenerated candidates config: 10,706 parents
in the 15:43-15:47 window, ZERO clamp hits on all three rails
(sigma cap, feedback ceiling, realized ceiling), session vol mult
constant at 1.498, regime mult 1.0. The window no longer contains a
large excursion under the floor-aware random path - the mid spanned
27,957 to 28,015, about 58 points - exactly the possibility the
amended claim scope anticipated, so this trace establishes ONLY that
the rails are untouched in an ordinary busy window and that the
instrument works end to end (raw t(4) innovations reached 20.5,
standardized 14.5, without any rail engaging). It does NOT explain
the per-seed envelope maxima; locating and tracing the new worst
minutes is the future tail-shape spec's first task, with this
instrument ready for it.

**Brick D2 - documentation** per 3.4, bundled.

RESULT, 2026-08-05, reviewed gate amendment for
`tape_lateness_under_acceleration`: during the loop's step-6 gate,
`brokkr check --gate` ran red on this one test - a wall-clock pacing
sample of the accelerated venue with a 50ms p99 bound - and paired
measurement established the failure as ENVIRONMENTAL, not a candidate
regression. Debug profile: parent 7fa473e fails at p99 106ms, the
candidate at 227-330ms. Release profile, N=5 each, minutes apart on
the same loaded box (load average 1.0, a second workspace building
intermittently): candidate 262/42/92/181/56ms with one pass, parent
261/43/249/257/291ms with one pass - indistinguishable distributions,
while the canonical oracle separately proves the crypto frame content
byte-identical. Both reviewers accepted: the candidate is no worse
than its parent under identical conditions, the 50ms release
threshold stays authoritative and unrelaxed, and the proper reading
of it is a quiet-box release run. The debug lane the gate uses is
unsuitable for this wall-clock bound; that mismatch is a recorded
work item in `notes/todo.md`, not a fix smuggled into this landing.

## 5. Out of scope, named

Reopen-gap implementation; any volatility mechanism change (trace
first, evidence second, spec third); the hourly session-profile
schema; shared crypto shape values (`SIZE_LOG_SIGMA`'s DEFAULT is the
shared value unchanged); `mnq06` and its decision contract; dynamic
width/displacement (14.4-A); MBO/queue; ES/MES evidence. The
teardown boundary: `SweepShape` construction and the size draw path
in `mogwai-data`, the offline gen summary/trace surface in
`mogwai-server/src/gen.rs`, the harness, and the version constant -
no live-server, wire-protocol, engine, or HTTP changes.

New `SUBCONTRACT_KEYS` entrants, as literal identifiers:
`SIGMA_GRID_POINTS`, `SIGMA_GRID_DOMAIN`, `RESAMPLE_SEED`,
`RESAMPLE_REPLICATES`, `RESAMPLE_SESSIONS_PER_REPLICATE`,
`RESAMPLE_ENVELOPE_LEVEL`, `MINUTE_RANGE_GATES`.
