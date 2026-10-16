# Generated-walk throughput: consolidated optimization report

Two independent source-reading reports (report A and report B, received
2026-10-16) on the three workloads that still bound the slow tests, merged
into one prioritized list of hypothetical wins. Where both raised the same
idea, the entry carries the union of their detail and says so; where only
one did, it names its source. Every finding is recorded as the reports
stated it. None was run or benchmarked by either report: every cost figure
is an estimate from reading the code, and every claim is to be investigated
before it is acted on. Where the two disagree, both positions are kept.

Below the new list, the open work this report already carried is retained:
the two rewrites from the earlier round that have not been started, the
items deliberately declined, and the state of the slow and failing tests.
The earlier round's landed work (priorities 1 to 4 of that round, tape
protocols 35 and 36, and its local changes) is in the git history and is
no longer described here.

## The three workloads

- The legacy main stream. `realism`, `session_edge_spike_lifts_realized_clamp`
  and `session_edge_spike_localizes` (`crates/mogwai-data/src/generated/tests.rs`)
  walk one fingerprint-fitted river (`xbtusd_anchor`: no arrival kernel, no
  cascade, no book, no calendar) through `next_tick`. Both reports confirm
  the legacy walk. Correction (both reports): `windowed_latent_returns`
  stops after `draw` parents, not ticks, so each session-edge walk is one
  million parents, about 9.5 million records (8.5M trades, 1M quotes).
  `session_edge_spike_lifts_realized_clamp` measures two such realizations
  plus a small determinism comparison. Priority 2 took the readers off
  these walks; what is left is parent resolution.
- The bare GARCH recursion. `shipped_garch_rails_sit_above_the_clean_tail`
  runs `measure_uncapped_tail`: 8 seeds of 2,000,000 dependent updates, one
  after another on one thread.
- The doubled MNQ day. Resolved by priority 1 below: the control now walks
  its day once.

Where the time goes, report B's estimates (release, per unit):

- Legacy generator about 400 to 500 ns per parent, of which size
  decoration is about 40% (17 size-stream u64, 8.5 AS 241 quantiles and 8.5
  exp per parent).
- `realism`'s `measure` about 200 ns or more per trade, about 4x the
  generator: a `TradeTick` with two Decimals and an `Arc` clone, a Decimal
  divide and `fract` for the off-grid check, two `decimal_to_f64`,
  `is_round_lot` recomputing `powf(log10(median))` every call, a SipHash
  `HashSet` insert, and two `owed_children()` calls each rescanning the
  rest of the plan.
- MNQ cascade walk about 250 to 350 ns per parent per pass, at about 1.16
  children per parent. `summarize` about 1 us per parent (a Decimal turned
  into a `String` key for a `BTreeMap<String, u64>` per trade, another map
  entry for the minute detail, two i128 civil-time segment lookups and a
  Decimal width per quote, two more Decimal-to-String keys, a `format!`
  displacement key, a heap `Vec` for levels and another segment lookup per
  parent). `GeneratedAcc` about 0.5 us per parent (two i128 segment lookups
  per print, four to six `BTreeMap` operations per parent). On those
  figures the two generator passes are about a quarter of the test.
- Bare GARCH about 25 to 35 ns per step.

Report B's headline: for the legacy and MNQ workloads the wall is probably
mostly the readers, not the generator, with measurement code at 3 to 5 times
the walk per parent in release. Report A's: substantial avoidable work
remains before the irreducible recursions are the whole story - control runs
two overly broad measurements, session-edge decorates sizes it never reads,
parent advancement expands records it discards, and rail sizing serializes
independent seeds. There is no lock or channel handoff in these hot loops
(report A); the serialization is stochastic dependency plus unrelated work
done through one mutable source.

## Priority 1. One walk for the control run, measuring only what control consumes (landed)

Landed 2026-10-16 (uncommitted at the time of writing), report A's endpoint,
sparred with codex to consensus. Exact for the tape.

What it is now. `control_walk` is one unsized walk into `ControlAcc`
(`arrival_control.rs`), which measures per-session blocks 1 to 3 and the
minute ranges and nothing else: no decoration, no trace, no block 4, no
forensics, no summary. The pieces it shares with the full measurements are
shared code, not copies:

- `GeneratedAcc` split into `SessionFrame<O: FrameObserver>` (window and
  lab-frame selection, rotation, book in force, the single close point,
  complete-session emission) and `Forensics`, the observer the frame calls
  synchronously at its print point and close point. `ControlAcc` is the same
  frame with a unit observer and `SessionAcc::close_blocks_1_to_3`.
- `MinuteRangeAcc` lifted out of `summarize` with its arithmetic unchanged;
  both `summarize` and `ControlAcc` use it.
- `ParentSource::next_parent_unsized` and `UnsizedParentView` in
  `mogwai-data`: the generator skips `decorate()`, and the view exposes only a
  quantity-free projection. `GeneratedAcc::walk` reads unsized too, so every
  full 12a measurement drops its size draws as well.
- `control_exposure` resolves the profile and source once for
  `control_walk` and `control_generated_pass`.

Measured. The real-day test went from about 23 s to 8.8 s alone in dev and
is no longer ignored; it runs in the plain lane.

The test's claim changed. The old pin (two passes agree on parent count)
had no content once there was one walk. The real-day test is now an
integration claim (the one session date, the exposed hour set as a set).
Numerical correctness is pinned on crafted streams with independently
written expectations: control blocks equal the full measurement on the
stream `measure12a_matches_independent_recompute` anchors; the two
independent selections (burn-in child, unsided print, lab-halt print,
straddling end); forensics delivered across a rotation forced from inside a
print, with the deferred share resolving in the next session; and a
mixed-reader generator test covering every transition out of an unsized
read. Each was bite-checked by a text-edit mutation.

Not done from the reports. Report B's pipelined multi-thread variant was not
built: one walk with one cheap measurement left nothing to overlap. Report
B's scratch-config versus preset profile equality pin was not added; the
control no longer compares the two paths, but the fit driver still resolves
through the scratch path.

Found on the way, open:

- The corrected-curve arm cannot run. The MNQ preset's calendar carries an
  activity envelope, and profile resolution refuses a `session.intensity_hour`
  override as inert, so `mogwai arrival-control` stops at the first test
  seed. Pinned by `the_corrected_curve_is_refused_under_the_shipped_envelope`
  and recorded in `docs/cli.md`. What the control should re-centre under an
  envelope needs an owner ruling.
- The same refusal makes `arrival_control_refuses_a_tree_that_changed_during_the_run`
  (`mogwai-cli`) fail with the wrong refusal: its mid-run hook sits after
  the test walks it can no longer reach. Not run here (it needs a clean
  tree); it follows the ruling above.
- `fit::walk::profile_from_config` formatted anyhow errors with `{e}`,
  dropping the cause chain, so the inert refusal surfaced as "configured
  symbol NVDA is invalid". Fixed to `{e:#}`. The `NVDA` label itself comes
  from `mogwai_venue::config::build_instrument_profiles`, which names a
  symbol-less boot shape by `DEFAULT_PRESET` even when an `[instrument]
  preset` selects another; the label misleads and is not fixed.
- Two incidental behaviour changes: a control window that is not a whole
  number of seconds is no longer refused (the duration string it fed is
  gone), and a walk with no in-window trade is now a recorded B4 refusal
  rather than an error out of the nearest-rank helper.

## Priority 2. Readers that use the compact records and assume time order (landed)

Landed 2026-10-16 (uncommitted at the time of writing), sparred with codex to
consensus and audited by a fresh codex session afterwards. Exact: no tape
byte, no statistic and no serialized output moves, so no bump is owed.

What it is now.

- The four latent helpers in `crates/mogwai-data/src/generated/tests.rs`
  (`durations`, `latent_returns`, `windowed_latent_returns`,
  `measure_session_curves`) step with `advance_parent` and read no record.
  `assert_placed` refuses a discrete-book walk, whose first trade is not at
  its parent instant. The windowed helper still classifies by the resolved
  end clock, not the parent instant. `discard_owed` is gone; its comment's
  claim that discarding moved later draws had been stale since tape 36.
  `measure_session_curves` reduces through `session_curves_from`, so the pin
  runs the real helper.
- `realism`'s `measure` reads sized `next_parent` views: integer off-grid
  check, a dense hour bitset, one parent boundary per view instead of two
  `owed_children` suffix scans per trade. Every float still goes through the
  `Decimal` route the old reader used (each size, and each parent's last
  trade price), so the compact reader is bit-identical, not merely within
  tolerance. The old reader stays as `measure_records`; both share one
  reduction, `summarize_measured`.
- `SegmentCursor` (`mogwai-lab` `session.rs`): remembers the last segment
  and answers from it while `origin <= ts < end`. Used by `SessionFrame`,
  `Forensics` and `summarize`. `ScheduleFrame::session_segment_at` has
  different boundary semantics and is untouched.
- `SessionAcc`'s three minute maps fused into one `MinuteCell` map behind a
  current-minute register that merges on revisit. A revisit is routine: a
  parent closes after the print that follows it, which can sit in the next
  minute. `block1` reads the fused keys; no minute carries a quote alone,
  because the count is written before the quote range.
- `summarize`: size and displacement histograms counted in `HashMap`s by
  integer key and rendered to the same string-keyed maps at the end;
  integer banker-rounded quote width, with the `Decimal` path kept for a
  tick the price scale cannot hold; horizon moments by index; the levels
  buffer reused; the worst-minute detail and per-minute parent counts behind
  a `MinuteMap` register; `rank_top_minutes` validates every minute, then
  selects 32 rather than sorting all of them.

Pins, each bite-checked by a text-edit mutation:
`compact_measurement_matches_the_record_reader` (every field, bit for bit,
60k parents); `parent_helpers_match_the_record_readers` (all four helpers'
outputs against record-reading references under clean, storm, drought, an
edge spike the walk crosses into, and a crossed reopen);
`segment_cursor_answers_what_the_function_answers` (every session boundary
plus or minus 1 ns under four offsets, both `u64` extremes, forward,
backward and scrambled); `a_revisited_minute_merges_rather_than_replaces`;
`integer_quote_widths_round_as_the_decimal_path_did` (every width to 40
ticks either way, ticks of 1 to 7 units, ties of both parities, inverted
books); `top_minutes_select_what_a_full_sort_keeps` (0, 31, 32, 33 and 80
minutes, ties across the cut, an unselected off-grid minute still refusing);
`a_minute_map_merges_a_revisited_minute`.

`summarize` was also checked once end to end: its serialized JSON
byte-identical to the pre-change version over MNQ, MES, NVDA and BTCUSDT,
seeds 1, 7 and 42, five-hour windows across the close and reopen, in
release. That used a temporary in-tree copy of the old function, deleted
afterwards; the unit pins above are what stays.

Measured.

- `session_edge_spike_lifts_realized_clamp` 24.3 s to 9.0 s, dev, alone.
- `realism` 35.0 s to 23.3 s in dev, alone; 1.8 s in release. Still past
  the 20 s watchdog in dev: the 2M parents' resolution alone is about that,
  so its
  `debug-slow` ignore stays until the generator gets cheaper (priority 3 or
  4). It fails exactly as before (`abs_return_acf_lag10` 0.1832), the same
  realization read the same way.
- The control walk's real-day test 8.8 s to 7.8 s in dev. Bound by the
  generator there.
- `summarize` and `GeneratedAcc` were not benchmarked through `brokkr
  mogwai`; the fit-driver and month-window win is unmeasured.

Not done from the reports, deliberately:

- Report B's integer round-lot test (`qty % lot_units == 0`) is not exact.
  The old predicate tests near-integers in float, and a size a hair off a lot
  multiple classifies differently under the two. The float predicate stays,
  with its lot hoisted.
- Report B's closed-form per-second counts per parent: the per-trade integer
  loop is already cheap, and the closed form buys nothing a bench could see
  behind the generator.
- Report A's streaming autocorrelations: the retained 2M-element series cost
  nothing measurable, and streaming changes the estimator's numerics.
- Report B's per-session minute arrays and two live forensic minutes: the
  register in front of one map is exact under any arrival order, the arrays
  are exact only under an ordering contract nobody states, and the forensic
  map is read back by the deferred share and by selection.

## Priority 3. A placed parent as a compact sweep description (landed)

Landed 2026-10-16 (uncommitted at the time of writing), sparred with a fresh
codex session to consensus and audited by another afterwards. Exact: no tape
byte moves, so no bump is owed.

What it is now.

- `emit_children` drains the sweep's level steps whole
  (`LevelSteps::drain`, the same gaps in the same order as asking every
  child) and resolves the sweep with `resolve_sweep` without laying a child
  out. The levels are monotone and the grid accepts a contiguous range of
  whole ticks, so the opening and final levels decide every child. On a
  refusal the children are replayed one by one, in floats, to find the
  refused child; near `2^53` the closed form is not trusted.
- A refused sweep leaves the per-child walk's state exactly: the clock at
  the refused child, and the end-of-sweep state (remembered price, drift)
  moved only when that child is the last, because `step_child` updated it
  before checking the last child's price. The one difference is the terminal
  stream position, since every gap is drawn before the refusal; a refusal is
  terminal, so nothing reads it.
- The resolved sweep is a `PlacedSweep` beside the plan. Record readers and
  sized parent views lay it out (`expand`). `advance_parent`, `skip_parent`
  and a seek dropping a whole parent never do, and `next_parent_unsized`
  returns a `SweepTrades`-backed view that computes the trades as it is
  iterated, each iteration fresh. It is cleared at every resolution attempt.
- `SweepTrades` is public but built only by a successful resolution, which
  has proven its invariants.

The independent reference stays in the tree, test-only: the per-child walk
(`emit_children_per_child`, `step_child`, `LevelSteps::steps_at`), selected
per source by a flag. It shares no code with the new resolver. Pins, each
bite-checked by a text-edit mutation:
`compact_sweeps_resolve_as_the_per_child_walk_did` (seven fixtures - legacy
clean, storm, drought, an edge spike, a reopen, the integrated kernel and
the placed cascade - with 6,000 parents each read through every reader in
rotation, including a clone after an unsized read, against the reference's
records and its full boundary state: clock, reopen frontier, child
reservation, stream position and continuation, latent bits, end-of-sweep
state); `a_compact_seek_lands_where_the_per_child_walk_lands`;
`draining_steps_is_asking_every_child` (forced p of 0 and 1, counts 0 and
1, last-trial steps and terminal overshoots, the stream's continuation as
well as its position); `a_resolved_sweep_is_the_per_child_walk` (the floor,
`2^53` from both sides, the mantissa overflow edge, refusals on the first,
a middle and the last child, saturated instants).

Measured, release, one binary with the reference flag as the A/B (per
million parents):

| Walk | `advance_parent`, per-child | compact | unsized view read twice, per-child | compact |
|---|---|---|---|---|
| legacy and its regimes | 204 ms | 168 ms | 215 ms | 209 ms |
| integrated | 220 ms | 184 ms | 230 ms | 224 ms |
| cascade | 278 ms | 240 ms | 291 ms | 287 ms |

About 35 ns per parent saved, 15 to 18 percent of a parent-skipping walk and
3 percent of an unsized read; trade-price sums identical between the arms.
The rest, about 170 ns per parent, is the serial core: the arrival draw,
the latent update, the side and book, the count and the step gaps. That is
priority 4's ground. Dev walls barely moved (`realism` 23.8 s,
`session_edge_spike_lifts_realized_clamp` 8.3 s, the control real-day test
8.8 s, all single noisy runs): the unoptimized core dominates there.

Not done: report A's run-aware accumulation for the control and 12a
readers, one update per price run instead of per print. Codex's audit:
window, lab-halt, session, minute and count-window boundaries can split a
sweep, and `GeneratedAcc` observes every print forensically, so it needs
its own design, and at 3 percent of an unsized read the view is not where
the time is.

## Priority 4. A serial feedback core with fixed draws per parent, and pure emission

Both reports (report A section 5, report B R3). Byte-moving: owes a
`TAPE_PROTOCOL_VERSION` bump, re-blessed goldens and re-run gates. Both say
to land it after the exact priorities above, which leave the walk as the
remaining cost and make this one's gain measurable.

Bottleneck. One ChaCha12 stream consumed in parent order, with a variable
amount per parent (report B): the ziggurat normal and the Marsaglia-Tsang
gamma inside `ChiSquared` reject; the child mixture takes one or two
uniforms; drift coins are drawn only in the high regime; the repeat coin only
for single-child low-regime parents; the level-step gaps vary with the sweep.
So nobody can know where parent k's draws start without running parents 0 to
k-1, every reader pays for everything, and nothing batches. The stream mixes
draws that feed back (gap, flip, innovation, side and regime, drift, count,
repeat, and on the bounce walks the number of level steps) with draws that
do not (where the steps fall; on the cascade, every step). Report A's framing:
the ordering serializes more than the mathematics requires; a standardized
innovation can be sampled before the previous variance is known, a unit
Weibull before its scale, uniforms for state-dependent decisions before their
thresholds.

Redesign.

- Core with a fixed K u64 per parent (report B), about 11 to 12 for the
  legacy walk: gap, flip, three for the innovation, two for side, two for
  drift (always drawn), one count, one repeat. The normal through the AS 241
  quantile already in the tree; chi-squared with 4 degrees of freedom is
  exactly `-2 ln(u1 u2)`, so `t(4) = Phi_inv(u0) / sqrt(-0.5 ln(u1 u2))`,
  three uniforms, no rejection, same law. The child mixture from one uniform:
  below q one child, else a geometric on the rescaled remainder. Keep the
  gamma path for odd `student_df`; even ones are `-2 ln` of a product.
- Level steps to a child-indexed stream like sizes (report B): a parent with
  c children draws at most c - 1 gaps, so each parent owns a contiguous
  range. The bounce walks need only the step count to recentre, since the
  per-step floor collapses to `max(first - s, 1)`; the cascade never feeds
  steps back, so its core skips them.
- Core output one compact row per parent (timestamp, side, first price, child
  count, first child index, regime, book); emission (records, step positions,
  sizes) a pure function of the row and the child-indexed streams (report B).
  Report A's equivalent: generate independent random inputs in separate
  domains, run the parent state updates in order, produce compact resolved
  parents, decorate child-index ranges independently on request, then reduce
  or expand.
- Batching (both): with a fixed draw count, a block's uniforms generate in one
  call and transform with SIMD (the central AS 241 branch is a ratio of
  polynomials; ln and exp vectorize), and the serial recursion runs over
  precomputed arrays. Report A: use reusable block streams, introducing random
  addressing only where independently scheduled chunks need it; never
  initialize or seek a generator per scalar draw.
- Parallel decoration (both): `SizeStream`'s fixed four-word stride and seek
  let workers generate disjoint child-index ranges, exposing the roughly 17M
  decorations of a 2M-parent `realism` walk. For `realism` the workers can run
  the production size law and quantizer and return moments and classification
  counts rather than protocol trades; consumers needing quantities get
  contiguous blocks (report A). Emission and per-record measurement split
  across cores by block; realism's parent-level series come from the core
  rows and its per-trade statistics are mergeable sums (report B).

Payoff. Readers needing only parents or the latent mid pay only the core:
session-edge, the dwell arms, the session curves, checkpoint extension and
venue seeks; report B guesses 60 to 100 ns per parent against 400 to 500 for
a full legacy read today. `realism` becomes bounded by the serial core. The
cascade gains less from emission at 1.16 children per parent, but still from
fixed-length samplers: 13 normals per open second and a t draw per parent.
Report A: plausible because expensive sampling and decoration leave the
serial chain, unmeasured, and if the remaining state update dominates, more
workers help little.

Risks. The tape bump and re-blessing (`clean_regime_is_byte_identical`, the
transcript, the size-draw heads), realism gates and fit tolerances re-run.
Distributions unchanged except the AS 241 tail truncation near 8.3 sigma
(already accepted for sizes) and a couple of bits lost in the rescaled
mixture uniform (report B). The real cost is downstream: the committed
generated-side artifacts, the 12a walk caches and the parity12a generated
seeds, are specific realizations and must be regenerated with month-scale
walks. `ARRIVAL_MEAN_CAL` was bisected on the seed-42 realism draw; the value
stays inside its 10% band but its provenance sentence must be reworded
(report B). Buffered state must distinguish generated-ahead from consumed for
cloning, checkpoints, faults and observation data (report A).

## Priority 5. Rail sizing as independent seeds (landed)

Landed 2026-10-16 (uncommitted at the time of writing), sparred with a fresh
codex session to consensus.

What it is now.

- `measure_uncapped_tail` walks each seed on its own scoped thread with its
  own generator, `GarchVol`, two tail collectors and squared-return partial,
  the real `GarchVol::step` and the per-step rail assertion unchanged; a
  worker's failed assertion re-raises at its join. Results fold in seed
  order.
- `TopK::merge` is exact, ties included (codex settled the tie worry: a
  collector with a finite threshold already holds `keep` values at or above
  it, so rejecting another equal one never leaves it short). Compaction
  partitions at `keep - 1`, so the pivot is the retained minimum and the
  threshold; queries sort the retained values once, which the envelope
  harness depends on, since it scans `values` directly after `max()`. The
  capacity stays 65,536: the envelope harness reports a count reaching it as
  censored, so shrinking it would change that diagnostic. `seen` sums the
  merged collectors' observations.
- `value_exceeded_by` is documented as the order statistic it is; "exceeded
  by exactly `rank` samples" was true only without ties.

The ranked tails, maxima and the envelope's counts are unchanged; the RMS is
a sum of per-seed partials and may move in its last bits. The `consts.rs`
figures reproduce: sigma 57.2x `vol_scalar`, largest clean return 3.33e-3,
RMS 1.2393e-5. Pinned by `a_tail_collector_keeps_what_a_full_sort_keeps`
(data dense in exact ties, whole and split-and-merged, short of capacity),
bite-checked with a wrong threshold.

Measured: `shipped_garch_rails_sit_above_the_clean_tail` 17.0 s to 2.8 s in
dev, alone, and back in the plain lane, where it passed under full-suite
load.

Not done: the fixed-draw t(4) sampler belongs to priority 4; seeds in SIMD
lanes and a parallel prefix scan are not worth it with one seed at about
2.8 s.

## Priority 6. The cascade's offspring as exponential intensity state

Report A. Full cascade rewrite. Conditional, lower confidence.

Bottleneck. In `cascade.rs` every parent samples Poisson offspring counts for
exponential delay kernels and inserts individual future timestamps into a
heap, against which later parents compete.

Redesign. Represent offspring as exponential intensity components that decay
between events and take an increment at each, replacing a population of
scheduled offspring with a small state vector. This attacks heap operations,
random memory access and explicit future-event materialization.

Conditional. A small pending heap may be cheap, and sampling the next event
under summed exponential intensities can itself be costly. Equivalence is
substantial: per-second immigrant buckets, closures, minimum event spacing,
placed-child clock advancement, pending-event handling and the heap ceiling
all shape the implemented process, and ordinary branching-to-intensity
equivalence does not automatically preserve them. Implement the complete
alternative and benchmark it against the full cascade workload and its
statistics, without routing flags around partial variants.

## Local changes

Medium value. Both reports would fold these into the rewrite that owns their
site rather than land them alone.

1. Cascade child spawning (report B). `spawn_children` builds a Poisson
   distribution three times per parent for fixed rates, three `exp` per parent
   for constants. Precompute in `CascadeTables`; building a distribution draws
   nothing, so no tape change.
2. Per-parent `Arc` clone (report B). `Arc::clone(&self.spec)` in each
   `begin_*` is an atomic increment and decrement per parent, there only for
   the borrow checker. Split the borrows.
3. Size stream (report B). Each child uses two u64; splitting one between the
   size uniform and the snap coin halves it. A counter-based generator would
   also remove the buffer regeneration on every seek. Either is a tape bump.
4. Cascade immigrant placement (report B). Each immigrant does a `powf`;
   drawing a second's immigrants at once (sorted uniforms, or normalized
   exponential spacings) avoids it. Tape bump.
5. Small re-derivations (report B). `LevelSteps::new` recomputes `ln_1p(-p)`
   every parent for one of three constants; `is_round_lot` recomputes its lot
   every call; `owed_children` is O(remaining).
6. Lab calendar lookups (report B). `open_throughout` scans the calendar's
   windows per call; the generator's `CompiledCalendar` minute table
   (crate-private today) could serve the lab.
7. A specialized Student-t sampler (report A), after work elimination and
   block execution. Payoff depends on sampler and hardware, and the cascade's
   degrees of freedom are configurable, so a hardcoded legacy sampler must not
   quietly become the cascade's. Priority 4's fixed-draw t(4) is report B's
   form of the same idea.

## Afterwards, if these work

- One parent-block producer feeding sinks as the runner for every lab walk:
  `arrival_screen`, `arrival_envelope`, the fit driver and `gen --type summary`
  (report B).
- Multi-seed `realism` (report B, needs an owner ruling): K arms of `DRAW/K`,
  each reporting mergeable sufficient statistics. Report B holds that the
  comment's "lag-50 autocorrelations need one continuous series" does not
  hold statistically - segments of 250k parents against a GARCH half-life of
  about 700 parents pool to essentially the same estimator - and that what is
  actually tied to the single seed-42 draw is `ARRIVAL_MEAN_CAL`'s provenance.
  Report A holds that lag 50 requires one continuous realization. Both
  positions stand until measured.
- Instruments built on many independent seeds (the dwell arms, rail sizing,
  envelope cells) running seeds in SIMD lanes (report B).

## Lateral findings from the two reports

- `session_edge_spike_localizes` is not ignored and runs in the fast lane
  (report B; true since tape 36 re-enabled it).
- Realism's off-grid check cannot trip on anything the walk does (report B):
  every record price is a whole number of ticks times the tick mantissa, so it
  only verifies the Decimal conversion at the edge.
- The `TopK` capacity was kept at 65,536 under priority 5, deliberately: the
  envelope harness's censoring diagnostic reads it. The control-walk laterals were
  closed by priority 1's rewrite; what it found in turn is listed there.

## Suggested sequence

Report B: measure first - time a bare walk against a walk with its readers for
all three workloads, registered as brokkr targets that report parents,
records, main-stream words and size-stream words, which settles whether the
readers dominate. Then priority 2 and re-measure; priority 1 and re-measure
(exact for the tape, but it changes the test's claim, an owner call); the
rail-sizing threads at any time; priority 4 as one complete intrusive change,
kept or reverted on the numbers. Report A's order puts the control rewrite
first, the compact sweep second, rail threads third and block execution
fourth, each a complete replacement benchmarked then kept or reverted, with
wall time and processor work both measured so parallelism is distinguishable
from eliminated work. Both: hold seeds, horizons and populations fixed where
the tape is preserved; give a tape-changing rewrite its own identity and
statistical validation; assess control throughput on the median-count day and
the wider seed set, never a lighter realization.

## Carried over: rewrites from the earlier round, not started

Neither addresses the three workloads above; both stand on their own merits.

### The discrete-book path as one compact transaction

Byte-preserving. Priority follows use: no shipped preset activates the book.
`resolve_book_parent()` once cloned the book configuration per parent (it now
borrows it through the shared spec); book execution still allocates a fill
vector, and the reporting split returns another, cloning the fills even when
no split occurs. Redesign: execute the parent into one compact transaction
buffer, integer fills plus pre and post book snapshots; reporting splits
modify it in place; quote suppression resolves into emission flags;
timestamps follow the reserved-slot schedule; wire records are built only for
a reader; wire emission and checkpoint advancement consume the same
representation. The earlier interim fix is moot: checkpoint extension no
longer builds a separate per-parent book plan. Risk: preserving the causal
chain among execution, depletion, reporting splits, impact, relaxation and
quote suppression.

### Shared generated blocks across delivery timelines

Byte-preserving in principle. A boat is shared among passengers with the same
boat key; the duplication is across speeds or placements: `Boatyard` obtains
separate positioned sources and each boat's `Tape` generates and serializes
independently, so overlapping spans are simulated and serialized repeatedly.
Redesign: each river gets a store of compact immutable record blocks plus
continuation checkpoints; boats own delivery cursors and pacing; history and
execution scans read the same blocks. Payoff scales with distinct span
demanded rather than the sum of overlapping reads; little benefit to a single
sequential consumer. Risks: a fast reader must not expose future information
to a slower one's market state; a slow reader must not force unbounded
retention; publishing blocks must not hold a broad reader lock through
simulation.

## Carried over: declined, with the reason

- Chi-squared closed form as a standalone change: byte-moving on its own
  (now folded into priority 4's fixed-draw core).
- A per-state sweep-shape cache: `begin_event` re-solves per event on purpose,
  and the code records a measurement showing no gain.
- Compressing a snapshot's `ChaCha12Rng` to key plus word position: about a
  megabyte per river at the checkpoint cap.
- Moving the few fixed scalars inside `ArrivalClock`, `BounceState`,
  `SweepShape` and `RegimeState` into the shared spec: a few words each.

## Carried over: slow and failing tests

### Ignored as debug-slow

Each carries an ignore reason beginning `debug-slow:`, so
`git grep -n "debug-slow:"` lists them; `brokkr check --gate` still runs
them. Walls are one test alone in a dev build, measured 2026-10-16.

| Test | File | Tape 35 | Tape 36 | After priority 2 |
|---|---|---|---|---|
| `realism` | `crates/mogwai-data/src/generated/tests.rs` | 37.8 s | 35.0 s, and fails (below) | 23.3 s (23.8 s after priority 3), 1.8 s in release, still fails |

`realism` is the only one left. It is over the watchdog on parent
resolution alone; priority 4 is what would bring it under.

Re-enabled at tape 36: `session_edge_spike_localizes` (12.5 s),
`one_walk_serves_every_probe_on_a_symbol` (12.5 s) and
`sweep_pass_walks_only_the_new_span` (7.5 s). Re-enabled by priority 1:
the control walk's real-day test, now
`the_control_walk_covers_every_exposed_hour_of_its_session` (8.8 s, was
about 23 s as `the_control_walk_pair_replays_one_tape`; 7.8 s after
priority 2). Re-enabled by priority 2:
`session_edge_spike_lifts_realized_clamp` (9.0 s, was 24.3 s). Re-enabled
by priority 5: `shipped_garch_rails_sit_above_the_clean_tail` (2.8 s, was
17.0 s).

The control walk's pinned day (seed 301) went from 667k parents at tape 35 to
1.28M at tape 36 while cost per parent fell; over seeds 300 to 316 the day
count runs from 0.50M to 2.37M with a median of 1.21M, so 301 is a typical
day and tape 35's was a light one.

### The realism gate misses its own band

`realism` (seed 42) fails `abs_return_acf_lag10` at 0.183 against the
fingerprint band's 0.1565 ceiling. Not a tape 36 regression: over seeds 0 to
15 in release, the lag-10 seed mean is 0.164 on tape 35 and on tape 36 alike,
about half the seeds above the ceiling on either, and every other realism
statistic's seed mean agrees between the tapes to about the third decimal.
The shipped BTCUSDT law sits above its own band at lag 10; seed 42 landed
inside it at tape 35 by chance. Fit work, owed to `notes/todo.md` and to a
comment at the test site; neither the gate nor the band has been touched.

### Failing under the gate, not timing out

Both bind a loopback listener, so only `brokkr check --gate` runs them, and
both fail an assertion rather than the watchdog. The first fails identically
on the tree before the tape 35 record rewrite.

| Test | File | Observed |
|---|---|---|
| `venue_announces_run_complete_and_exits_zero_at_the_declared_sim_deadline` | `crates/mogwai-cli/tests/completion.rs` | serves about 1.968 s of a declared 2 s, below the 1.98 s `boat_skew_floor` |
| `a_short_accelerated_run_is_not_over_before_it_is_ready` | `crates/mogwai-cli/tests/completion.rs` | failed in the same gate run; not yet examined alone |

The shortfall is about 32 ms on every run, and `elapsed_ns` counts from the
boat's placement, so the likely cause is a slower placement (the warmup reach)
in an unoptimized dev build. Not yet confirmed.

### Stale measurements

- The near-watchdog list predates tape 36 and has not been re-measured. Under
  `brokkr check --timings` load it read: the wire-frame oracle
  (`crates/mogwai-cli/src/gen.rs`, then `protocol9_tape_oracle`) 14.6 s; nine
  `crates/mogwai-venue/src/fills.rs` tests between 13.5 s and 14.5 s
  (`a_resident_window_serves_a_miss_walk_free_and_walk_identical`,
  `a_walk_stops_once_every_scan_has_triggered`,
  `a_truncated_drain_reports_where_it_stopped`,
  `a_pass_costs_one_walk_per_symbol_not_per_order`,
  `stop_touch_scans_and_limit_through_scans_share_one_walk`,
  `every_configured_symbol_gets_its_own_chain`,
  `the_deciding_prints_are_the_prints_trades_serves`,
  `last_trade_at_or_before_never_looks_ahead`,
  `triggers_only_on_prints_strictly_through_the_trigger`); and
  `boatyard::tests::two_boats_do_not_evict_each_other_s_market_reading` 14.0 s.
  The fills and boatyard group clustered at one wall, suggesting a shared setup
  walk, which tape 36 shortened.
- The shared-spec and compiled-calendar change (before tape 36) was never
  benchmarked on the calendar presets' seeks, history and checkpoints.
