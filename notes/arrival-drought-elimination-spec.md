# Arrival-drought elimination - implementation spec

Written against `reference/technical-implementation-spec.md`. Spawned from the
`notes/todo.md` entry "BUILD: eliminate arrival droughts from the default tape;
keep the drought as an armable havoc scenario" (decided 2026-08-01). The
mechanism background lives in `reference/architecture.md` ("Tape arrival
droughts"); this spec does not restate it beyond what each brick needs.

## 1. The item

The fitted default tape prints 15-18 h near-silent stretches that real BTCUSD
never prints, because the ACD duration memory decays per TICK: a high-psi
excursion persists ~1/(1-phi) = ~154 ticks regardless of how long each tick
takes in wall time, so hour-scale gaps self-prolong into days. Neither
committed duration target (dispersion band, duration ACF) constrains
wall-clock dwell, so the realism gate is structurally blind to the deserts.

Done means:

1. The default tape's wall-clock dwell is bounded by a target measured from
   the real Kraken anchor series, not chosen by judgement.
2. The bound is enforced by the realism gate itself (`measure()` gains dwell
   statistics, asserted), so the tape can never desert again silently.
3. The committed duration bands (dispersion, ACF) still hold - the fix goes
   through the mechanism and a retune, never through a serving-side cap.
4. The dying-symbol shape stays available, armable via
   `MarketRegime::LiquidityDrought`, and is pinned by a test.
5. The golden stream is re-blessed once, and every doc describing droughts as
   ambient default behavior is rewritten.

Explicitly excluded, named as separate TODOs: the AD12 dead-feed watchdog and
penetration-gated fills (both unblocked by this landing, built after it), and
the `next_position` accumulation item.

## 2. Survey of the ground

### 2.1 The duration mechanism (`crates/mogwai-data/src/generated/`)

`source.rs` `next_duration_ns` is the entire arrival process:

- eps: `Weibull(1.0, ACD_WEIBULL_SHAPE=0.60)` draw, normalized by
  `WEIBULL_MEAN_SHAPE_060` so its mean is 1.
- recursion: `psi = omega + alpha * prev_duration_s + beta * psi`, applied
  once per tick. `alpha = ACD_PERSISTENCE * ACD_FEEDBACK_SHARE`
  (0.9935 * 0.08), `beta = ACD_PERSISTENCE - alpha`,
  `omega = mean_duration_s * (1 - ACD_PERSISTENCE)`, so the unconditional
  mean of psi is `scalars.mean_duration_s` (the fingerprint's fitted ~7 s).
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

### 2.2 The gate (`crates/mogwai-data/src/generated/tests.rs`)

- `realism()`: seed 42, `DRAW = 2_000_000` ticks, `xbtusd_anchor` scalars,
  `start_ts 0`, no regime. `measure()` derives EVERY duration statistic from
  consecutive differences of realized `trade.ts_event` - it never sees the
  internal `duration_s`. Asserted today: dispersion in the committed band
  [131.7 .. 4608.9] (seed 42 lands ~190, near the floor, per the
  `consts.rs` comment); duration ACF lag1/lag5 within
  `DURATION_ACF_ABS_TOL = 0.14` of the anchors 0.194 / 0.136; the return,
  zero-change, round-lot, size and on-grid gates. `Measured` has twelve
  fields; none is a function of wall-clock density.
- `clean_regime_is_byte_identical` pins the golden tick sequence; the
  checkpoint tests pin byte-identical resumed tails; the `#[ignore]`d
  `session_curves` test (5M ticks) pins the session envelope.
- `liquidity_drought_stretches_durations` pins thin_factor 5 stretching the
  mean gap at least 4x.

Every byte-pinned expectation above changes when the recursion changes; the
re-bless set is enumerated in L2.

### 2.3 The offline pipeline (`analysis/`)

- `characterize.py`: one streaming pass per corpus CSV, emits
  `char_<PAIR>.json` with a `duration` dict (`mean_s`, `var_s2`,
  `dispersion_index`, `log_hist`, `acf`). Tracks `first_ts`/`last_ts`
  already. Memory O(1) over multi-GB files - the new dwell stats must keep
  that property.
- `run_corpus.py` fans over `DEFAULT_PAIRS` (8 pairs, pool capped at 6);
  `build_fingerprint.py` assembles `analysis/fingerprint.json` from the
  `char_*.json` reports (anchor XBTUSD, cross-pair min/median/max bands).
- The corpus lives at `MOGWAI_DATA_DIR` (default
  `/media/folk/Banan/Kraken_Trading_History`), offline-analysis input only.
  L1 requires that disk mounted; nothing else in this spec does.
- `fingerprint.json` is embedded via `include_str!` and parsed by
  `fingerprint.rs`. The serde structs do not set `deny_unknown_fields`, so a
  new JSON key parses cleanly before any Rust reads it - this is what makes
  L1 land without touching the crates.

### 2.4 Blast radius outside the generator

- `scalars.mean_duration_s` and every non-duration target are untouched, so
  tick-count budgets hold: `MAX_HISTORY_SEEK_TICKS 190_000`,
  `CHECKPOINT_K 8192`, the server's 24 h `backfill_horizon_ns`, and the
  smoke's window sizes all stay valid.
- The adapter's tape-sparsity warning on an empty warmup window stays: it is
  still the correct diagnosis under an ARMED drought.
- `trades_window_is_clamped_at_sim_now` (widened to 6 h because of the
  deserts) stays at 6 h; tightening it is a follow-up nicety, not this spec.
- `scripts/smoke.py` anchors on a real tape tick and is density-robust.
- Docs describing droughts as ambient: `reference/architecture.md` ("Tape
  arrival droughts"),
  and the drift-comment in `dynamics.rs` / session comments that cite the
  byte-identical golden stream as the reason not to touch the walk (those
  reasons survive; only the drought section's "plan for it" framing dies).

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
  what dies is only the wall-clock compounding of the tail.
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

**D3 - the dwell target is measured from the corpus, and the gate reads the
ANCHOR, not the cross-pair band.** This is deliberately unlike the dispersion
band: the cross-pair spread includes near-dead pairs whose dwell is exactly
the behavior being evicted from the default tape, so a cross-pair max would
gate nothing. The fingerprint records both (anchor + range, matching every
other target's shape, and the range documents what the havoc knob may
imitate); `realism()` asserts against the anchor values only.

**D4 - the dwell gate is one-sided.** Two statistics, both upper bounds:

- `max_gap_s`: the largest realized inter-trade gap in the draw.
- `empty_hour_frac`: the fraction of whole simulated hours in the draw's
  span containing zero trades.

Upper bounds only, because the failure mode is silence, and the lower side is
already policed: the dispersion floor (131.7) requires the big-gap mass to
exist, so a tape too dense to be credible fails the existing band. Slack: the
gate asserts `max_gap_s <= 2.0 * anchor.max_gap_s` and
`empty_hour_frac <= anchor.empty_hour_frac + 0.01`, the factor-of-two and
absolute point covering seed wobble on a 2M draw the same way
`DURATION_ACF_ABS_TOL` does; both slack constants live next to it in
`tests.rs` with this rationale.

**D5 - retune procedure and levers.** Order of operations: set tau to land
the dwell bounds first (tau directly bounds `max_gap_s`; start at 1800 s and
move by factors of 2), then restore any degraded duration statistic:
dispersion low -> raise `ACD_PERSISTENCE` toward 0.995 or lower
`ACD_WEIBULL_SHAPE` toward 0.55 (both fatten realized variance); ACF low ->
raise `ACD_FEEDBACK_SHARE`. Acceptance is the full `realism()` gate at seed
42 / 2M draws; during tuning also spot-check seeds 7 and 1337 with the same
`measure()` harness (not asserted - the committed gate stays seed 42, per the
existing convention) so the landed constants are not a seed-42 accident. The
`consts.rs` block comment is rewritten to document the new joint tuning
story: persistence/shape land dispersion+ACF, tau lands dwell.

**D6 - feasibility, stated so failure is recognizable.** The anchor series
itself satisfies dispersion, duration ACF, and realistic dwell
simultaneously - the joint target is achievable by construction. Open is only
whether THIS parametric family reaches it. Back-of-envelope says yes:
dispersion ~190 at 7 s mean needs realized variance ~1300 s^2, which ~200
hour-scale gaps per 2M draws deliver without any day-scale desert. If an
honest search (D5's levers, a day of iteration) cannot land all bands plus
dwell, L2 is REVERTED whole and the todo entry reopens with the failed
constants recorded - that evidence, not judgement, is what would justify
revisiting the serving-path split rejected in D1.

**D7 - `thin_factor` bounds stay [1.0, 1000.0].** On the dense default tape,
thin 1000 is a ~2 h mean gap with much longer clustered excursions - the
dying-symbol shape the decision preserves. No protocol change; the scenario
is pinned by a new test (4.6) rather than by new surface.

## 4. Target artifacts

### 4.1 `analysis/characterize.py`

The `duration` dict gains three keys, all computed in the existing single
streaming pass, O(1) memory except one bitset:

- `max_gap_s`: running max of the inter-trade gap.
- `empty_hour_frac`: hours-with-zero-trades over total span hours. Track a
  `set` (or bytearray bitmap) of `ts // 3600` indices seen; corpus spans are
  ~10 years = ~90k hour slots, bounded and small. Denominator is
  `(last_ts - first_ts) // 3600`.
- `gap_p999_s`: 99.9th percentile gap, read off the existing `log_hist`
  duration histogram (no new pass) - recorded for the fingerprint's
  documentation value, not gated on.

The stdout summary line gains `max_gap` and `empty_hours` so a corpus run
shows the dwell story at a glance.

### 4.2 `analysis/build_fingerprint.py`

`golden_targets` gains:

    "dwell": {
      "max_gap_s":       { "anchor": ..., "range": {min, median, max} },
      "empty_hour_frac": { "anchor": ..., "range": {min, median, max} },
      "gap_p999_s":      { "anchor": ..., "range": {min, median, max} },
      "_doc": "gate reads the ANCHOR; the cross-pair range documents the
               dying-symbol spread the LiquidityDrought regime imitates"
    }

Assembled from the reports exactly like the existing targets. The summary
print gains the anchor dwell numbers.

### 4.3 `analysis/fingerprint.json`

Regenerated by rerunning `run_corpus.py` then `build_fingerprint.py` with the
corpus disk mounted. Every EXISTING target must reproduce byte-for-byte
modulo float formatting - the code computing them is untouched; a diff
showing an existing band moved means the corpus or code drifted and stops the
landing. The only additions are the `dwell` block and per-pair `duration`
keys inside the regenerated `char_*.json` intermediates.

### 4.4 `crates/mogwai-data/src/generated/fingerprint.rs`

`GoldenTargets` gains `pub dwell: DwellTargets` (struct of three
anchor+range fields, same shape as `duration_dispersion_index`). Lands in L2
(serde tolerates the unread key during L1).

### 4.5 `crates/mogwai-data/src/generated/consts.rs` and `dynamics.rs` and `source.rs`

- `consts.rs`: new `ACD_WALL_RELAX_TAU_S: f64` with a doc comment giving
  D2's story; the ACD block comment rewritten per D5; the three ACD constants
  updated to the retuned values.
- `dynamics.rs`: `AcdClock` gains `mean_s: f64`.
- `source.rs`: `try_with_clamp_override` seeds `mean_s: mean_duration_s`;
  `next_duration_ns` replaces the recursion line with D2's two lines. The
  "order is load-bearing" comment block is extended one sentence: the
  relaxation weight reads `prev_duration_s` (last tick's un-modulated draw),
  never the realized gap.

### 4.6 `crates/mogwai-data/src/generated/tests.rs`

- `Measured` gains `max_gap_s` and `empty_hour_frac`; `measure()` computes
  them from the same `timestamps` vector (max of `durations`; hour-bucket
  scan of the span).
- `realism()` gains the two one-sided asserts of D4 against
  `fp.golden_targets.dwell`, with the two slack constants and their rationale
  comment.
- Re-bless in place: `clean_regime_is_byte_identical`'s pinned sequence, the
  checkpoint tests' pinned tails, and any literal-bearing session/gap test
  the new stream shifts. The re-bless is expected and lands in the same
  commit as the mechanism - a green suite with the old bytes would mean the
  mechanism change is not wired.
- New `liquidity_drought_imitates_dying_symbol`: thin_factor 1000, ~50k
  ticks, asserts the realized mean gap lands in the hours (>= 3600 s and
  <= 4 * 3600 s * thin-scaled slack) and that `max_gap` clumps well past the
  mean (>= 5x mean gap) - pinning that the havoc knob reproduces the shape
  the default tape no longer prints.

### 4.7 Documentation

- `reference/architecture.md` "Tape arrival droughts": rewritten from "a
  consequence any consumer has to plan for" to: the default tape carries a
  corpus-anchored dwell bound (state the gate), droughts are the
  `LiquidityDrought` regime's job, the two operational consequences move
  under an "under an armed drought" framing, and the "fingerprint-refit
  decision" closing paragraph dies.
- `reference/havoc.md` `LiquidityDrought` bullet gains the dying-symbol
  framing and a pointer to the pinned test.
- `notes/todo.md`: the drought entry is REMOVED entirely (its unblock notes
  for AD12 and penetration fills already live in those entries).

## 5. Landings

### L1 - measure the corpus (analysis + fingerprint only)

Artifacts 4.1, 4.2, 4.3. No crate changes. This is the measurement-first
landing: it prices the premise before the mechanism is touched.

Proceed/close threshold: the premise is "real BTCUSD never deserts". If the
regenerated anchor report shows `max_gap_s >= 21600` (6 h) or
`empty_hour_frac >= 0.05` on XBTUSD, the premise is false, the item is
mispriced, and L2 is never laid - the decision reopens with the numbers.
Expected reading, for calibration: minutes-scale max gaps and
`empty_hour_frac` ~0.

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
the retuned constants, the dwell assertions, the golden re-bless, and the doc
rewrites are inseparable (each without the others is either a red suite or a
lie about the tape).

Gates, exact commands:

    brokkr check
    brokkr test -p mogwai-data realism
    brokkr test -p mogwai-data clean_regime_is_byte_identical
    brokkr test -p mogwai-data liquidity_drought
    brokkr test -p mogwai-data session_curves

(`liquidity_drought` substring-matches both the existing stretch test and the
new dying-symbol test; `session_curves` is `#[ignore]`d and slow, and the
envelope interacts with realized gaps, so it runs explicitly here.)

Advisory, not a gate: `python3 analysis/plot_tape.py --gen --type bars
--interval 1h --length 4d --open` - the chart that surfaced the decision
should now show no grey empty-hour bars on the default profile.

Keep/revert: kept when all five commands are green. If D5's search cannot
land the joint gate, the WHOLE landing reverts (constants, mechanism, gate,
docs) and the todo entry reopens per D6 - no half-kept state where the
mechanism landed but the gate is loosened to accommodate it.

## 6. Ordering argument

L1 before L2 is forced twice over: L2's gate asserts against fingerprint
fields L1 creates, and L1's threshold can close the item. The suite is green
at the L1/L2 boundary because serde ignores the unread `dwell` key and no
generator byte moves in L1. There is no orderable smaller unit inside L2: a
mechanism change without the re-bless is red, a re-bless without the retune
pins a desert tape, a dwell assert without the mechanism is red by
construction (today's constants fail it - measured four ways already).

## 7. Stopping rule

Out of scope, stated flat: the AD12 watchdog and penetration-gated fills
(separate TODOs, unblocked by this landing); any change to
`thin_factor` bounds, wire types, or the control plane; the server's
synthesis limits (`MAX_HISTORY_SEEK_TICKS`, `CHECKPOINT_K`, horizons) - the
mean cadence is unchanged so their budgets hold; the adapter, including its
sparsity warning (still correct under an armed drought); `scripts/smoke.py`;
tightening `trades_window_is_clamped_at_sim_now` back below 6 h; the
`closed_window_gap_ns` session path (its budget walk consumes un-modulated
seconds and is untouched by the relaxation); and the offline
`KrakenCsvSource` lineage. The teardown stops at the ACD recursion: GARCH,
bounce/drift, sizes, sessions, and regimes keep their exact draw order so the
re-bless is a re-seeding of expectations, not a redesign of the walk.
