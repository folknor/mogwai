# Arrival-drought elimination - implementation spec

Written against `reference/technical-implementation-spec.md`. Spawned from the
`notes/todo.md` entry "BUILD: eliminate arrival droughts from the default tape;
keep the drought as an armable havoc scenario" (decided 2026-08-01). The
mechanism background lives in `reference/architecture.md` ("Tape arrival
droughts"); this spec does not restate it beyond what each brick needs.

Revised 2026-08-02: folds the validated findings of
`notes/arrival-drought-review-r1.md` and `notes/arrival-drought-review-r2.md`.
Section 8 records what was rejected and why.

Revised 2026-08-02 (adjudication, after the first L2 attempt): D5's stage-1
grid exhausted against the FULL-SPAN dispersion floor (best tuple 86.8 vs
131.7), and the adjudication found the floor itself dishonest rather than the
mechanism: the era-windowed anchor dispersion is ~36, not 4608.9 - the
full-span duration targets are dominated by the same infancy/outage deserts
D3 already disqualified for dwell. D3a below re-derives the dispersion band
and duration ACF anchor era-windowed; L2 restarts at D5 stage 1 against the
corrected targets. Section 8 records the ruling.

## 1. The item

The fitted default tape prints 15-18 h near-silent stretches, because the ACD
duration memory decays per TICK: a high-psi excursion persists
~1/(1-phi) = ~154 ticks regardless of how long each tick takes in wall time,
so hour-scale gaps self-prolong into days. Neither committed duration target
(dispersion band, duration ACF) constrains wall-clock dwell, so the realism
gate is structurally blind to the deserts.

The premise, stated carefully because the committed corpus complicates it: the
FULL-SPAN anchor series does contain hour-to-day-scale gaps. Decoding the
committed `char_XBTUSD.json` duration histogram (no corpus disk needed;
`analysis/decode_dwell_bins.py` is the helper) shows ~3050 gaps of an hour or
more over the 4194-day span, 94 of them in the top, SATURATING bin (>= 15 h,
unbounded above - `log_bin` folds everything past 24 h into it). Those are
venue-history artifacts: Kraken's 2013-2015 infancy and its outage record
(including a ~48 h total shutdown in January 2018), an era the default
profile does not claim to model. The default tape claims a PRESENT-DAY liquid
major, and the modern era of the anchor is the behavior to hold it to:
quiet regimes exist, deserts do not. So the dwell target is measured from an
era-windowed anchor - the era boundary is a declared constant
(`DWELL_ERA_START_TS`, 2019-01-01 UTC), and everything inside it is measured,
not judged.

Done means:

1. The default tape's wall-clock dwell is bounded by a target measured from
   the modern-era window of the real Kraken anchor series.
2. The bound is enforced by the realism gate itself (`measure()` gains dwell
   statistics, asserted), and the asserted gate covers the seed the served
   default tape actually runs (the server keys each symbol's walk on an
   FNV-1a hash of the symbol - the BTCUSDT tape is NOT seed 42), so the
   default tape can never desert again silently.
3. The committed duration bands (dispersion, ACF) still hold - the fix goes
   through the mechanism and a retune, never through a serving-side cap.
4. The dying-symbol shape stays available, armable via
   `MarketRegime::LiquidityDrought`, and is pinned by a test that checks the
   clustering survives thinning, not just that gaps stretch.
5. The golden stream is re-blessed once, and every doc describing droughts as
   ambient default behavior is rewritten.

Explicitly excluded, named as separate TODOs: the AD12 dead-feed watchdog and
penetration-gated fills (both unblocked by this landing, built after it), and
the `next_position` accumulation item.

## 2. Survey of the ground

### 2.1 The duration mechanism (`crates/mogwai-data/src/generated/`)

`source.rs` `next_duration_ns` is the entire arrival process:

- eps: `Weibull(1.0, ACD_WEIBULL_SHAPE=0.60)` draw, normalized by
  `WEIBULL_MEAN_SHAPE_060` (`numeric.rs`, shape-specific, pinned by
  `weibull_mean_matches_known_constant`) so its mean is 1.
- recursion: `psi = omega + alpha * prev_duration_s + beta * psi`, applied
  once per tick. `alpha = ACD_PERSISTENCE * ACD_FEEDBACK_SHARE`
  (0.9935 * 0.08), `beta = ACD_PERSISTENCE - alpha`,
  `omega = mean_duration_s * (1 - ACD_PERSISTENCE)`, so the unconditional
  mean of psi is `scalars.mean_duration_s`.
- `duration_s = psi * eps`, floored at 1 ns; `prev_duration_s = duration_s`
  BEFORE any modulation - the recursion never sees the session envelope or an
  armed regime. This invariant ("ACD feedback sees the un-modulated
  duration") is load-bearing and survives this spec.
- realized gap: `duration_s / arr_mult * regime.arrival_thin` on the
  open-market path, or the hour-integrating `closed_window_gap_ns` budget
  walk when the gap opens below `SESSION_CLOSED_ARR_MULT`. Both are
  downstream of the feedback store.

`dynamics.rs` `AcdClock` holds `omega/alpha/beta/psi/prev_duration_s/
eps_mean`. No RNG lives in the clock; `GeneratedSource` is `Clone` and its
future is a pure function of state - the substrate of `CheckpointIndex` seek.

`regime.rs`: `MarketRegime::LiquidityDrought { thin_factor }` sets
`arrival_thin`; the open path multiplies the realized gap by it. It is a
whole-tape regime carried per subscription on `Subscribe` and per request on
`GET /trades` (`mogwai-protocol/src/havoc.rs`), validated `[1.0, 1000.0]`,
never armed via `/control/divergence`, so no divergence-window ceiling
applies to it.

Cadence caveat that matters for dwell absolutes: `GeneratorScalars` (both
`xbtusd_anchor` and the server's `from_fingerprint_medians`) seed
`mean_duration_s` from the CROSS-PAIR MEDIAN (7.19 s), while the anchor's own
full-span mean is 4.44 s - the tape runs ~1.6x slower than the anchor by
construction (and the modern-era anchor mean will be lower still). Any gate
comparing realized dwell against anchor absolutes must scale by this cadence
ratio explicitly; a flat slack that silently absorbs it is not allowed.

### 2.2 The gate (`crates/mogwai-data/src/generated/tests.rs`)

- `realism()`: seed 42, `DRAW = 2_000_000` ticks, `xbtusd_anchor` scalars,
  `start_ts 0`, no regime. `measure()` derives EVERY duration statistic from
  consecutive differences of realized `trade.ts_event` - it never sees the
  internal `duration_s`. Asserted today: dispersion in the committed band
  (full-span [131.7 .. 4608.9] when this spec was written; era-windowed per
  D3a after the adjudication - seed 42 landed ~190 under the old constants);
  duration ACF lag1/lag5 within
  `DURATION_ACF_ABS_TOL = 0.14` of the anchors 0.194 / 0.136; the return,
  zero-change, round-lot, size and on-grid gates. `Measured` has twelve
  fields; none is a function of wall-clock density. Note on the dispersion
  band's lower side: the 131.7 floor is DOTUSD's number, not the anchor's, so
  it polices "too dense to be credible" only weakly - seed 42 sits near it,
  and the ACF band does most of the lower-side work.
- The gate's seed is NOT the production seed: the server derives each
  symbol's seed from an FNV-1a hash (`seed_for` in
  `mogwai-server/src/source.rs`), so the served BTCUSDT walk is a different
  realization from every committed test. Nothing today asserts anything about
  the tape broadarrow actually consumes.
- `clean_regime_is_byte_identical` pins the golden tick sequence in literal
  strings - it is the ONLY test carrying pinned stream bytes. The checkpoint
  tests (`checkpoint_resume_is_byte_identical` and siblings) compute a fresh
  from-origin reference from the live generator at runtime and compare the
  resumed tail against it, so they follow any mechanism change with no
  re-bless. The `#[ignore]`d `session_modulation_reproduces_curves` test (5M
  ticks) asserts measured session curves against the fingerprint's profile -
  re-run, not re-blessed.
- `liquidity_drought_stretches_durations` pins thin_factor 5 stretching the
  mean gap at least 4x.

The re-bless set is therefore exactly: the golden literals in
`clean_regime_is_byte_identical`, plus any literal-bearing session/gap test
the new stream shifts. It is enumerated in L2.

### 2.3 The offline pipeline (`analysis/`)

- `characterize.py`: one streaming pass per corpus CSV, emits
  `char_<PAIR>.json` with a `duration` dict (`mean_s`, `var_s2`,
  `dispersion_index`, `log_hist`, `acf`). Tracks `first_ts`/`last_ts`
  already. Memory O(1) over multi-GB files - the new dwell stats must keep
  that property. `log_bin` saturates: any gap >= 86400 s lands in the top
  bin, so the committed histograms cannot say how long the longest gaps are.
- What the committed reports already show about dwell (decoded, full span):
  XBTUSD is the MOST desert-prone pair in the corpus - 3050 gaps >= 1 h and
  top-4-bin counts [551, 298, 142, 94], against ETHUSD [135, 87, 52, 53],
  XDGUSD [157, 46, 13, 7], and single digits for ADAUSD/DOTUSD/SOLUSD/
  XRPUSD/USDTUSD. The full-span dispersion anchor 4608.9 IS the cross-pair
  band max. Both facts are era artifacts (XBTUSD has the longest history,
  reaching deepest into the infancy/outage years), and both are why the
  dwell measurement is era-windowed rather than full-span.
- `run_corpus.py` fans over `DEFAULT_PAIRS` (8 pairs, pool capped at 6);
  `build_fingerprint.py` assembles `analysis/fingerprint.json` from the
  `char_*.json` reports (anchor XBTUSD, cross-pair min/median/max bands) and
  ALSO regenerates the tracked `analysis/findings.md` human-readable summary
  on every run.
- The corpus lives at `MOGWAI_DATA_DIR` (default
  `/home/folk/Kraken`), offline-analysis input only.
  L1 requires that disk mounted; nothing else in this spec does.
- `fingerprint.json` is embedded via `include_str!` and parsed by
  `fingerprint.rs`. The serde structs do not set `deny_unknown_fields`, so a
  new JSON key parses cleanly before any Rust reads it - this is what makes
  L1 land without touching the crates.

### 2.4 Blast radius outside the generator

- `scalars.mean_duration_s` and every non-duration target are untouched, so
  tick-count budgets hold: `MAX_HISTORY_SEEK_TICKS 190_000`,
  `CHECKPOINT_K 8192`, the server's 24 h `backfill_horizon_ns`, and the
  smoke's window sizes all stay valid. This claim is now GUARDED rather than
  assumed: `measure()` gains an asserted realized mean-gap statistic (4.6),
  because the D2 relaxation does not preserve the mean exactly (D2's
  caveat) and the budgets above all price ticks-per-wall-hour.
- The adapter's tape-sparsity warning on an empty warmup window stays: it is
  still the correct diagnosis under an ARMED drought.
- `trades_window_is_clamped_at_sim_now` (widened to 6 h because of the
  deserts) stays at 6 h; tightening it is a follow-up nicety, not this spec.
- `scripts/smoke.py` anchors on a real tape tick and is density-robust; that
  includes the `--command-latency` step, which asserts command ordering rather
  than tape content.
- `Divergence::CommandLatency` stamps the market price after its ACT sleep, so
  a denser tape only makes the "the venue acted on a moved market" story it
  models truer. Nothing on that path pins a price or a gap literal, so it is
  outside the re-bless set.
- One server test is ALREADY a live casualty of the ambient drought:
  `subscribe_beyond_sim_now_clamps_to_a_live_stream` anchors on the real
  wall clock and asserts the first live trade prints within 1 h of sim-now.
  On 2026-08-02 the BTCUSDT walk's desert put the first trade ~1 h 12 min
  out and the suite went red on an untouched tree. Wall-time-dependent, so
  it self-heals when the clock exits the desert; the L2 dwell bound is what
  makes it deterministic again. Known, not fixed here - it is the defect's
  own evidence.
- Docs describing droughts as ambient: `reference/architecture.md` has THREE
  sites, not one - the "Tape arrival droughts" section, the reconnect/
  generation discussion's "a generation became current DURING a tape arrival
  drought" clause, and the bars/history discussion's "inside one of the
  tape's arrival droughts a request for N bars typically..." clause. All
  three become wrong under this landing. The drift-comment in `dynamics.rs`
  and the session comments cite the byte-identical golden stream as the
  reason not to touch the walk - those reasons survive; only the drought
  section's "plan for it" framing dies. `notes/todo.md`'s AD12 entry also
  leans on the old framing ("the drought ELIMINATION still gates it") and
  resolves against the landed dwell bound.

## 3. Design decisions, resolved here

**D1 - the fix acts inside the fitted mechanism.** The gate measures realized
gaps, so any cap applied outside the mechanism (serving path, or feeding the
ACD an uncapped value while emitting a capped one) lowers the statistics the
gate validates while pretending not to. Rejected permanently.

**D2 - wall-time relaxation of the ACD memory.** The recursion becomes:

    w     = exp(-prev_duration_s / ACD_WALL_RELAX_TAU_S)
    psi   = mean_s + w * (omega + alpha * prev_duration_s + beta * psi
                          - mean_s)

with `mean_s = scalars.mean_duration_s` (the recursion's unconditional mean
today) and `ACD_WALL_RELAX_TAU_S` a new fitted constant in `consts.rs`.
Properties, each load-bearing:

- Degenerates to today's recursion as tau grows (w -> 1). At the ~7 s bulk
  cadence and tau of O(10^3 s), w = 0.99x: ordinary clustering dynamics are
  perturbed by under a percent and the retune absorbs it.
- Kills the self-prolonging desert: each hour-scale gap collapses the memory
  toward the mean by `exp(-gap/tau)`, so an excursion's WALL dwell is bounded
  by a few tau regardless of how many ticks it spans. The tick-domain
  clustering the ACF band demands survives (it lives in the sub-minute bulk);
  what dies is only the wall-clock compounding of the tail. Stated honestly:
  tau bounds the PERSISTENCE of an excursion (the empty-hour statistics), not
  any single draw - `psi * eps` keeps an unbounded Weibull tail, so the
  one-draw maximum is bounded only in distribution, which is why the gate's
  hard asserts are quantile- and run-based rather than a sample-max (D4).
- Mean caveat, stated so the retune is honest about it: with constant `w` the
  fixed point is exactly `mean_s` for any `w`, but `w` is correlated with the
  state it damps (a long `prev_duration_s` produces a small `w`), so
  `E[psi]` sits below `mean_s` by a Jensen-style term that grows as the gap
  distribution fattens. The retune absorbs it - which means the landed
  `ACD_*` values are COMPENSATING constants, no longer the raw fit; the
  `consts.rs` comment must say so, and the realized mean-gap assert (4.6) is
  what keeps the compensation honest.
- `w` is computed from the UN-MODULATED `prev_duration_s`, preserving the
  invariant that the recursion never sees the session envelope or an armed
  regime. This is sufficient - desert gaps are hour-scale before modulation
  on the 24/7 default profile - and it is also what keeps an armed
  `LiquidityDrought` honest: `arrival_thin` stretches only the realized gap,
  so a thinned tape keeps the fitted clustering stretched intact instead of
  relaxing itself back to density. The dying-symbol scenario depends on this.
- Deterministic, no RNG consumed, no new state beyond one `mean_s` field on
  `AcdClock` and the const: `Clone`/checkpoint semantics unchanged.

Alternatives rejected: a hard per-draw duration cap (truncates the tail the
dispersion band needs, and bounds nothing - 154 capped 2 h gaps is still
13 days of desert); expressing the decay as `beta^(gap/mean_s)` (same family,
but couples the dwell horizon to the persistence constant, and the two must
be tunable independently - dispersion wants phi high, dwell wants the memory
horizon short).

**D3 - the dwell target is the ERA-WINDOWED anchor.** Two choices folded into
one decision:

- Era window: dwell statistics are computed only over trades at or after
  `DWELL_ERA_START_TS = 1_546_300_800` (2019-01-01T00:00:00Z), a named
  constant in `characterize.py` recorded in the fingerprint. Rationale: the
  full-span anchor is the most desert-prone series in the corpus (2.3), but
  its deserts are infancy/outage history, and the default profile claims a
  modern liquid major. The window is the declared judgement; everything
  inside it is measured. The boundary is load-bearing and named as such:
  pull it back to 2017 and the outage-era dwell re-enters the target, so
  relitigating the era means moving the declared constant, never pretending
  the corpus chose it. A gap belongs to the trade that CLOSES it: a gap is
  in-window iff its closing trade's timestamp is >= the era start (so the
  outage that STRADDLES the boundary is excluded only if it closes before
  it - deterministic and stated).
- Anchor, not the cross-pair band - but for the honest reason: the default
  tape claims to BE the anchor symbol, so the anchor's modern era is the
  behavior it must reproduce. (The previous draft's reason - "the cross-pair
  spread includes near-dead pairs whose dwell is the behavior being
  evicted" - is factually inverted on full-span data and is retired.) The
  fingerprint records both anchor and range, matching every other target's
  shape; the cross-pair range documents the dying-symbol spread the
  `LiquidityDrought` knob may imitate; `realism()` asserts against the
  anchor values only.

**D3a - the DURATION targets follow the era window (added by the 2026-08-02
adjudication).** The dispersion band and the duration ACF anchor were fitted
over the full corpus span; the dwell measurement (0c0a0b9) disqualified that
span for gap statistics, and dispersion and duration ACF ARE gap statistics.
Measured consequences, from the committed in-window dwell histograms and the
regenerated reports: the anchor's full-span dispersion 4608.9 collapses to
~36 in the modern window - the committed band max was almost entirely
desert-era variance - and the old floor 131.7 (full-span DOTUSD) sits far
ABOVE the modern anchor, so the stage-1 retune was failed against exactly
the behavior this spec evicts. The fix mirrors D3: `characterize.py` computes
in-window duration variance/dispersion and an in-window duration ACF over the
same gap population as the dwell block (a gap belongs to the trade that
closes it); `build_fingerprint.py` points `duration_dispersion_index` (anchor
and cross-pair range) and `duration_acf_anchor` at the windowed values.
Return, size and session targets stay full-span: they are per-tick shape
statistics, not gap statistics, and nothing disqualified their span. The
gate's Rust side is untouched - it reads the fingerprint. The full-span
values remain in the char reports as documentation.

**D4 - the dwell gate: quantile and run statistics, cadence-scaled,
one-sided.** The gated statistics, chosen for cross-sample-size stability
(the corpus window holds tens of millions of gaps, the test draw 2M - a
sample MAX is not comparable across that ratio, so `max_gap_s` is recorded
but never asserted):

- `gap_p999_s`: the 99.9th-percentile inter-trade gap. Corpus side: read
  from a NEW `dwell_hist` (160 log bins over [1 s, 604800 s), top bin
  saturating, populated only in-window) at the UPPER edge of the first bin
  where the cumulative fraction reaches 0.999 - biased conservatively high
  by at most one ~8.7% bin width; the read must not land in the saturated
  bin (if it does, the measurement itself fails loudly). Generator side:
  exact nearest-rank (`ceil(0.999 * n)`) over the draw's durations.
- `empty_hour_frac`: zero-trade hours over the span. Exact rule, identical
  in Python and Rust and pinned by a small hand-built fixture on each side:
  the population is every complete UTC hour bucket `[k*3600, (k+1)*3600)`
  lying fully inside the observation span (corpus: `[max(first_ts,
  DWELL_ERA_START_TS), last_ts]`; generator: first to last emitted
  `ts_event`); the numerator is the buckets containing zero trades. A span
  shorter than one complete hour defines the statistic as 0 (the corpus
  path can never hit this; the rule exists so the Rust helper is total).
- `max_empty_hour_run_h`: the longest run of CONSECUTIVE empty hours in the
  same population - the direct desert statistic, and the shape the 1m/15m
  charts surfaced.

Asserts in `realism()`, all upper bounds, with
`cadence = scalars.mean_duration_s / dwell.mean_s.anchor` (the windowed
anchor mean recorded in the fingerprint; the ~1.6x-or-more handicap of 2.1
made explicit instead of absorbed):

    measured.gap_p999_s        <= DWELL_P999_SLACK * cadence * anchor.gap_p999_s
    measured.empty_hour_frac   <= anchor.empty_hour_frac + EMPTY_HOUR_FRAC_SLACK
    measured.max_empty_hour_run_h <= anchor.max_empty_hour_run_h + EMPTY_HOUR_RUN_SLACK_H

with `DWELL_P999_SLACK = 2.0`, `EMPTY_HOUR_FRAC_SLACK = 0.01`,
`EMPTY_HOUR_RUN_SLACK_H = 2`, living next to the asserts in `tests.rs` with
this rationale (the slack covers seed wobble on a 2M draw plus the residual
population mismatch - the draw runs ~160 simulated days under the session
envelope, the window spans years). Upper bounds only, because the failure
mode is silence; the lower side is policed by the ACF band and (weakly) the
dispersion floor, per 2.2.

Also asserted, two-sided, guarding the budget claims of 2.4 against D2's
mean caveat:

    (measured.mean_gap_s - scalars.mean_duration_s).abs()
        <= MEAN_GAP_REL_TOL * scalars.mean_duration_s

with `MEAN_GAP_REL_TOL = 0.10`.

**D5 - retune procedure: a deterministic grid, not a manual search.** Two
implementers must land the same constants, so the search is a finite grid
with a stated order and a first-hit-wins rule:

- Stage 1, `ACD_WEIBULL_SHAPE` FROZEN at 0.60. Frozen for two coupled
  reasons: lowering the shape fattens the eps tail and directly re-inflates
  the realized gap quantiles tau just suppressed (the levers fight), and the
  shape is normalized by the shape-specific `WEIBULL_MEAN_SHAPE_060` in
  `numeric.rs`, so moving it drags a derived constant and its pinning test
  along. Grid, iterated in this nested order, outermost first:
  `ACD_WALL_RELAX_TAU_S` in [7200, 3600, 1800, 900] (descending: prefer the
  weakest relaxation - the dynamics closest to today's fit - that passes);
  `ACD_PERSISTENCE` in [0.9935, 0.9945, 0.9950];
  `ACD_FEEDBACK_SHARE` in [0.08, 0.10, 0.12].
  The winner is the FIRST tuple where the full `realism()` gate (all existing
  bands plus D4's dwell and mean-gap asserts) passes at seed 42 AND the
  dwell/mean-gap asserts pass at the production BTCUSDT seed (4.6). During
  tuning also spot-check seeds 7 and 1337 with the same `measure()` harness
  (not asserted - the committed gate stays per convention) so the landed
  constants are not a seed accident.
- Stage 2, only if stage 1 exhausts: `ACD_WEIBULL_SHAPE = 0.55`, which
  REQUIRES recomputing the unit-mean normalizer (Gamma(1 + 1/shape)),
  renaming/updating the `numeric.rs` constant, its doc comment, its imports,
  and the `weibull_mean_matches_known_constant` literal - the coupled
  artifact set, named here so it cannot be forgotten. Re-run the stage-1
  grid.
- Stage 3: no tuple passes - revert per D6.

The `consts.rs` block comment is rewritten to document the joint tuning
story: persistence/feedback land dispersion+ACF, tau lands dwell, the landed
values are compensating (D2's mean caveat), and the grid above is the
procedure that selected them.

**D6 - feasibility, stated so failure is recognizable.** The claim "the
anchor series satisfies dispersion, duration ACF, and realistic dwell
simultaneously" is now a claim about the ERA-WINDOWED anchor, and it is not
assumed - L1 measures it, and L1's threshold (section 5) closes the item if
the windowed anchor itself deserts. Given L1 passes, what remains open is
only whether THIS parametric family reaches the joint target.
Back-of-envelope says yes: dispersion ~190 at 7 s mean needs realized
variance ~1300 s^2, which ~200 hour-scale gaps per 2M draws deliver without
any day-scale desert. If the D5 grid exhausts, L2 is REVERTED whole and the
todo entry reopens with the failed grid recorded - that evidence, not
judgement, is what would justify revisiting the serving-path split rejected
in D1.

**D7 - `thin_factor` bounds stay [1.0, 1000.0].** On the dense default tape,
thin 1000 is a ~2 h mean gap with much longer clustered excursions - the
dying-symbol shape the decision preserves. No protocol change; the scenario
is pinned by a new test (4.6) that checks the CLUSTERING survives thinning
(the constant multiplier leaves the realized-gap ACF invariant, which is
exactly D2's un-modulated-feedback invariant made testable), rather than by
new surface.

## 4. Target artifacts

### 4.1 `analysis/characterize.py`

New module constant `DWELL_ERA_START_TS = 1_546_300_800` (2019-01-01 UTC)
with D3's rationale. The report's `duration` dict gains a `dwell` sub-dict,
all computed in the existing single streaming pass, O(1) memory except one
bounded bitmap:

- `era_start_ts`: echo of the constant (provenance in the report).
- `n_gaps`: in-window gap count (denominator provenance for the quantile).
- `mean_s`: in-window mean gap - the cadence-ratio denominator of D4.
- `max_gap_s`: running max of the in-window gap. Recorded for the
  fingerprint's documentation value, never gated on (sample-size unstable,
  D4).
- `gap_p999_s`: read off the new `dwell_hist` per D4's upper-edge rule.
- `dwell_hist`: 160 log bins over [1 s, 604800 s), top bin saturating,
  in-window gaps only (the existing coarse `log_hist` stays as-is,
  full-span).
- `empty_hour_frac` and `max_empty_hour_run_h`: per D4's exact complete-hour
  rule, tracked as a `set` (or bytearray bitmap) of `ts // 3600` indices
  seen in-window; the window spans ~7 years = ~65k hour slots, bounded and
  small.

The stdout summary line gains `max_gap`, `p999` and `empty_hours` so a
corpus run shows the dwell story at a glance.

### 4.2 `analysis/build_fingerprint.py`

`golden_targets` gains:

    "dwell": {
      "era_start_ts":        ...,
      "mean_s":              { "anchor": ..., "range": {min, median, max} },
      "max_gap_s":           { "anchor": ..., "range": {min, median, max} },
      "gap_p999_s":          { "anchor": ..., "range": {min, median, max} },
      "empty_hour_frac":     { "anchor": ..., "range": {min, median, max} },
      "max_empty_hour_run_h":{ "anchor": ..., "range": {min, median, max} },
      "_doc": "era-windowed; gate reads the ANCHOR (p999/frac/run, scaled by
               the cadence ratio against mean_s); max_gap_s is documentation;
               the cross-pair range documents the dying-symbol spread the
               LiquidityDrought regime imitates"
    }

Assembled from the reports exactly like the existing targets. The summary
print gains the anchor dwell numbers, and the regenerated
`analysis/findings.md` gains a dwell section (that file is rewritten by every
`build_fingerprint.py` run, so it moves in L1 whether or not we mean it to -
mean it to).

### 4.3 `analysis/fingerprint.json`

Regenerated by rerunning `run_corpus.py` then `build_fingerprint.py` with the
corpus disk mounted. Every EXISTING target must reproduce byte-for-byte
modulo float formatting - the code computing them is untouched; a diff
showing an existing band moved means the corpus or code drifted and stops the
landing. The additions are the `dwell` block, the per-pair `duration.dwell`
keys inside the regenerated `char_*.json` intermediates, and the
`findings.md` dwell section.

### 4.4 `crates/mogwai-data/src/generated/fingerprint.rs`

`GoldenTargets` gains `pub dwell: DwellTargets` (era scalar plus five
anchor+range fields, same shape as `duration_dispersion_index`). Lands in L2
(serde tolerates the unread key during L1).

### 4.5 `crates/mogwai-data/src/generated/consts.rs` and `dynamics.rs` and `source.rs`

- `consts.rs`: new `ACD_WALL_RELAX_TAU_S: f64` with a doc comment giving
  D2's story; the ACD block comment rewritten per D5 (including the
  compensating-constants admission); the ACD constants updated to the
  grid-selected values.
- `dynamics.rs`: `AcdClock` gains `mean_s: f64`.
- `source.rs`: `try_with_clamp_override` seeds `mean_s: mean_duration_s`;
  `next_duration_ns` replaces the recursion line with D2's two lines. The
  "order is load-bearing" comment block is extended one sentence: the
  relaxation weight reads `prev_duration_s` (last tick's un-modulated draw),
  never the realized gap.
- Stage 2 only (D5): `numeric.rs`'s Weibull mean constant, comment, imports
  and test move together with the shape.

### 4.6 `crates/mogwai-data/src/generated/tests.rs`

- `Measured` gains `mean_gap_s`, `max_gap_s`, `gap_p999_s`,
  `empty_hour_frac` and `max_empty_hour_run_h`; `measure()` computes them
  from the same `timestamps` vector (mean/max/nearest-rank quantile of
  `durations`; complete-hour scan of the span per D4's rule, with a
  hand-built fixture test pinning the hour rule against the Python
  definition).
- `realism()` gains D4's asserts (three one-sided dwell bounds, the
  two-sided mean-gap band) against `fp.golden_targets.dwell`, with the slack
  constants and their rationale comment.
- New `default_symbol_tape_dwell_is_bounded`: the production BTCUSDT walk.
  Seed is the FNV-1a-64 of "BTCUSDT" - the five-line fold is duplicated in
  the test with a comment naming `mogwai-server`'s `seed_for` as the source
  of truth (mogwai-data cannot depend on the server crate); scalars are
  `GeneratorScalars::from_fingerprint_medians("BTCUSDT", ..)` with
  `modal_tick`/`price_decimals` overridden to the default instrument's
  values (0.01 / 2), mirroring the server's `default_profile`. 2M draw,
  `start_ts 0`, asserting the dwell and mean-gap statistics only - this is
  the assert that makes done-means item 2 true of the tape actually served.
- Re-bless in place: `clean_regime_is_byte_identical`'s pinned literals, and
  any literal-bearing session/gap test the new stream shifts - and nothing
  else: the checkpoint tests recompute their references (2.2) and follow
  automatically. The re-bless is expected and lands in the same commit as
  the mechanism - a green suite with the old bytes would mean the mechanism
  change is not wired.
- New `liquidity_drought_imitates_dying_symbol`: thin_factor 1000, ~50k
  ticks. Asserts: realized mean gap in [3600 s, 14400 s] (thin 1000 on the
  ~7 s cadence is ~2 h, the band covers seed wobble); realized-gap ACF lag1
  within `DURATION_ACF_ABS_TOL` of the anchor lag1 (a constant multiplier
  leaves the ACF invariant, so this pins that the fitted clustering
  survives thinning - the claimed dying-symbol shape, not just stretched
  gaps); and `max_gap >= 5 * mean_gap` (the clustered-excursion tail
  exists).

### 4.7 Documentation

- `reference/architecture.md`: all THREE drought-as-ambient sites (2.4) are
  rewritten - the "Tape arrival droughts" section becomes: the default tape
  carries a corpus-anchored dwell bound (state the gate), droughts are the
  `LiquidityDrought` regime's job, the two operational consequences move
  under an "under an armed drought" framing, and the "fingerprint-refit
  decision" closing paragraph dies; the generation-during-drought clause and
  the bars-request-inside-a-drought clause are reworded to the armed-drought
  framing.
- `reference/havoc.md` `LiquidityDrought` bullet gains the dying-symbol
  framing and a pointer to the pinned test.
- `notes/todo.md`: the drought entry is REMOVED entirely (its unblock notes
  for AD12 and penetration fills already live in those entries), and the
  AD12 entry's "the drought ELIMINATION still gates it" clause is updated to
  cite the landed dwell bound as the threshold source.

## 5. Landings

### L1 - measure the corpus (analysis + fingerprint only)

Artifacts 4.1, 4.2, 4.3. No crate changes. This is the measurement-first
landing: it prices the premise before the mechanism is touched.

Proceed/close threshold: the premise is "the modern-era anchor never
deserts". If the regenerated WINDOWED anchor report shows
`empty_hour_frac >= 0.01`, or `max_empty_hour_run_h >= 6`, or
`gap_p999_s >= 3600`, the premise is false, the item is mispriced, and L2 is
never laid - the decision reopens with the numbers. Expected reading, for
calibration: `gap_p999_s` in the tens of seconds to low minutes,
`empty_hour_frac` ~0, `max_empty_hour_run_h` 0-2 (outage residue), and a
`max_gap_s` possibly in the low hours - which is fine, because max_gap is
documentation, not a gate (D4).

Gates, exact commands:

    python3 analysis/characterize.py XBTUSD
    python3 analysis/run_corpus.py
    python3 analysis/build_fingerprint.py
    brokkr check

`brokkr check` proves the fattened JSON still parses into the unchanged
structs (`fingerprint_parses`) and the realism gate still passes untouched -
the suite is green at this boundary because nothing reads `dwell` yet.

### L2 - mechanism, retune, gate, re-bless, docs

Artifacts 4.4, 4.5, 4.6, 4.7, as ONE coherent landing: the recursion change,
the grid-selected constants, the dwell assertions, the golden re-bless, and
the doc rewrites are inseparable (each without the others is either a red
suite or a lie about the tape).

D3a addendum: the L2 landing ALSO carries the regenerated
`analysis/fingerprint.json` and `analysis/findings.md` (one
`python3 analysis/run_corpus.py` then `python3 analysis/build_fingerprint.py`
run - the analysis code computing the windowed duration targets is already
landed). The corrected duration targets are red against the desert-era
constants by construction (seed 42's duration ACF lag1 measures 0.306 against
the windowed anchor 0.160, outside the 0.14 tolerance - the defect measured a
fifth way), so the fingerprint regeneration is inseparable from the retune
for the same reason the dwell asserts are. For grid calibration: the
corrected dispersion band is [36.3 .. 1627.9] cross-pair, windowed anchor
36.3, windowed duration ACF anchor lag1 0.1603 / lag5 0.1113; the previously
failed stage-1 dispersion 86.8 is comfortably inside the corrected band.

Gates, exact commands:

    brokkr check
    brokkr test -p mogwai-data realism
    brokkr test -p mogwai-data clean_regime_is_byte_identical
    brokkr test -p mogwai-data default_symbol_tape_dwell_is_bounded
    brokkr test -p mogwai-data liquidity_drought
    brokkr test -p mogwai-data session_modulation_reproduces_curves
    brokkr test -p mogwai-server subscribe_beyond_sim_now_clamps_to_a_live_stream

(`liquidity_drought` substring-matches both the existing stretch test and the
new dying-symbol test; `session_modulation_reproduces_curves` is the exact
name of the `#[ignore]`d 5M-tick session test - an earlier draft wrote
`session_curves`, which matches ZERO tests and would have been a gate that is
green because it ran nothing; the envelope interacts with realized gaps, so
it runs explicitly here.)

`subscribe_beyond_sim_now_clamps_to_a_live_stream` is RED on an untouched
tree before this landing, for the reason the blast-radius survey records: it
anchors on the real wall clock, and the ambient desert put the first live
trade past its 1 h bound. It is gated here because the survey claims the L2
dwell bound is what makes it deterministic again, and an unverified claim
about the landing's own central effect is exactly what a gate is for. It is
also the ONLY check in the tree that judges the SERVED tape, at the
production FNV seed, against the real clock rather than a sampled statistic
- so it is the closest thing to end-to-end evidence this item can produce.
Read it together with `default_symbol_tape_dwell_is_bounded`: because the
test is wall-time-dependent it could in principle go green by the clock
wandering clear of a desert, so green here counts as landing evidence only
while the dwell assertions are also green. Still red after the retune means
the item failed, not that the clock was unlucky.

Advisory, not a gate: `python3 analysis/plot_tape.py --gen --type bars
--interval 1h --length 4d --open` - the chart that surfaced the decision
should now show no grey empty-hour bars on the default profile.

Keep/revert: kept when all six commands are green. If D5's grid exhausts,
the WHOLE landing reverts (constants, mechanism, gate, docs) and the todo
entry reopens per D6 - no half-kept state where the mechanism landed but the
gate is loosened to accommodate it.

## 6. Ordering argument

L1 before L2 is forced three times over: L2's gate asserts against
fingerprint fields L1 creates, L1's threshold can close the item, and D5's
grid needs L1's anchor numbers to evaluate its first tuple. The suite is
green at the L1/L2 boundary because serde ignores the unread `dwell` key and
no generator byte moves in L1. There is no orderable smaller unit inside L2:
a mechanism change without the re-bless is red, a re-bless without the retune
pins a desert tape, a dwell assert without the mechanism is red by
construction (today's constants fail it - measured four ways already).

## 7. Stopping rule

Out of scope, stated flat: the AD12 watchdog and penetration-gated fills
(separate TODOs, unblocked by this landing); any change to
`thin_factor` bounds, wire types, or the control plane; the server's
synthesis limits (`MAX_HISTORY_SEEK_TICKS`, `CHECKPOINT_K`, horizons) - the
mean cadence is unchanged (now asserted, 4.6) so their budgets hold; the
adapter, including its sparsity warning (still correct under an armed
drought); `scripts/smoke.py`; tightening
`trades_window_is_clamped_at_sim_now` back below 6 h; the
`closed_window_gap_ns` session path (its budget walk consumes un-modulated
seconds and is untouched by the relaxation); and the offline
`KrakenCsvSource` lineage. `numeric.rs` is touched only if D5 reaches stage
2. The teardown stops at the ACD recursion: GARCH, bounce/drift, sizes,
sessions, and regimes keep their exact draw order so the re-bless is a
re-seeding of expectations, not a redesign of the walk.

## 8. Review disposition

Both review rounds were validated against the code and the committed
`char_*.json` data before folding. Folded (with where): the corpus-contains-
deserts finding and the anchor-is-most-desert-prone inversion (r1) -> the
era-windowed premise and D3's rewritten rationale; the `session_curves`
zero-match gate (r1, r2) -> L2's command list; the tau/shape lever coupling
and the tau-bounds-persistence-not-max correction (r1, r2) -> D2/D5; the
sample-max incomparability and p999 promotion (r1, r2) -> D4; the cadence
mismatch (r1) -> 2.1 and D4's explicit ratio; the Jensen mean drift (r1,
r2) -> D2's caveat and the asserted mean-gap band; the ungated production
seed (r2) -> done-means 2 and `default_symbol_tape_dwell_is_bounded`; the
Weibull normalizer coupling (r2) -> D5 stage 2; the non-reproducible retune
(r2) -> D5's grid; the imprecise `empty_hour_frac`/percentile definitions
(r2) -> D4's exact rules; the unpinned drought-test clustering and undefined
"thin-scaled slack" (r2) -> 4.6's ACF-invariance assert and explicit band;
the overstated re-bless set and the forgotten `findings.md` (r2) -> 2.2 and
4.2; the incomplete doc sweep (r1) -> 2.4 and 4.7.

Rejected:

- r1's conclusion that the item closes under its own L1 threshold ("do not
  lay L1; the item is mispriced"). The data behind it is correct and is now
  IN the spec, but the conclusion does not follow: the corpus deserts are
  infancy/outage artifacts of an era the default profile does not claim, so
  the fix is scoping the measurement to the claimed era, not closing the
  item. The threshold survives, rewritten against the windowed anchor.
- r1's implied move off the anchor (since the anchor is the loosest dwell
  gate available). The anchor stays: the default tape claims to BE the
  anchor symbol, and the era window - not a different pair - is what evicts
  the dead-era dwell.
- r2 finding 1's fallback arm ("or the completion claim must be narrowed").
  The stronger arm was taken instead: the production seed is asserted.
- The 2026-08-02 adjudication of the first L2 attempt. The implementer ran
  D5 stage 1 faithfully, found the strongest tuple at dispersion 86.8 against
  the committed floor 131.7, and reverted per the keep/revert rule - correct
  procedure. The adjudication ruled the FLOOR wrong, not the mechanism: the
  era-windowed anchor dispersion is ~36 (the full-span 4608.9 was desert-era
  variance), so 86.8 is on the dispersed side of the modern anchor and the
  failure was manufactured by a full-span target the era window had already
  discredited. Disposition: D3a lands as an L1-style analysis/fingerprint
  amendment; L2 restarts at D5 stage 1 (shape still frozen at 0.60) against
  the corrected band and windowed ACF anchor; stage 2 (unfreeze the shape,
  drag `WEIBULL_MEAN_SHAPE_060` along) remains the fallback exactly as
  written. Considered and not taken: unfreezing the shape FIRST (treats a
  symptom of a wrong target as a tuning problem, and the levers fight per
  D5's own rationale), and abandoning the ACD family (no evidence yet that
  the family fails against an honest target).

- r1's nit that `AGENTS.md` still calls `docs/` "the transient TODO" while
  its own folder table defines `docs/` as durable usage docs. Real, but a
  root-convention-file fix with no connection to this spec; not folded.
