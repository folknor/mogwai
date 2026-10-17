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

Landed 2026-10-16, report A's endpoint,
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
  seed. Recorded in `docs/cli.md` and filed in `notes/todo.md`. What the
  control should re-centre under an envelope needs an owner ruling.
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

Landed 2026-10-16, sparred with codex to
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

Landed 2026-10-16, sparred with a fresh
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

## Priority 4. A serial feedback core with fixed draws per parent, and pure emission (fixed core landed; batching not)

Landed 2026-10-16 as tape protocol 37,
for the two bounce walks only; sparred with fresh codex sessions to
consensus, the step stream re-sparred after a measured regression, and the
whole audited afterwards.

What it is now.

- A legacy parent draws exactly twelve main-stream words and an integrated
  one seven (its clock and count are the kernel's, on the cadence stream),
  one per slot in a fixed order (`generated/draws.rs`), every slot drawn
  whatever branch is taken; the results are applied in the unchanged causal
  order. The transforms reject nothing: the gap is `-ln` of an open uniform,
  a coin a uniform below its probability, the innovation
  `Phi_inv(u0) / sqrt(-0.5 ln(u1 u2))`, the count its branch and geometric
  words. The laws are the old ones in the continuous limit, not on the 52-
  and 53-bit grids (the exponential's zero atom and its cap near 36.7, coin
  quantization at `2^-53`, a bounded t(4) support).
- The bounce walks' level-step gaps left the main stream for a counter-based
  `StepStream`: the `j`-th gap of a sweep whose first child is `c0` is
  SplitMix64 at counter `c0 + j`. A ChaCha stream addressed the same way was
  built first and measured 19 ns per parent, nearly all of it generating
  reserved words no parent read; the counter form computes only what is read
  (under 1 ns).
- The cascade and the discrete book are untouched and byte-identical, which
  `the_cascade_walks_hash_to_their_blessed_bytes` now pins: nothing else
  hashed a cascade walk.
- The V4 and V5 conformance vectors' contract-A order is now an exact
  twelve-slot list, checked against the drawing code through a counting word
  source and against the measured stride of a 20,000-parent walk (the first
  version checked only the stride; codex's audit caught it, and a swap inside
  `LegacyDraws::draw` now fails it).

Re-blessed: `clean_regime_is_byte_identical`, `analysis/tape-oracle-hashes.json`,
the fill-distribution golden, the stage-A manifest's version and digest.
The fill golden needed a harness fix first: under the band, the new tape
gives one order a partial fill on arrival, and the harness counted fill
events, so that order was two samples and the shape assertion refused. It
now counts an order once, at the fill that completes it.

Gates on the new tape: every plain-lane statistical gate (the eight dwell
arms, `sweep_structure`, the floor-branch and surge tests);
`session_modulation_reproduces_curves`; `realism` passes at seed 42, but the
16-seed lag-10 mean is 0.1628 against 0.164 on the old tapes, 11 of 16 seeds
above the 0.1565 ceiling - the same law, and seed 42 is luck. The mean gap
is 0.1747 s against the declared 0.17104 s; `ARRIVAL_MEAN_CAL`'s provenance
comment now says its bisection realization is gone and the gate holds it.

Measured, release, per million parents: legacy about 164 ms, integrated
about 188 ms, against about 168 and 184 before (this host's run-to-run
noise is about 10 percent). Break-even. In isolation the new t(4) costs 20 ns
against 27 for the ziggurat-plus-gamma, and the twelve ChaCha words 26 ns per
parent; those and the state update are now the irreducible per-parent work
in scalar code.

Not done, and why it matters: the speedup report B estimated (60 to 100 ns
per parent) was never in the fixed draws themselves; it is in what they
enable, below. Still open:

- Block generation and SIMD transforms. A block's 12 words per parent can be
  generated and transformed in vector lanes (AS 241 is a ratio of
  polynomials; `ln`, `sqrt`, `exp` vectorize). A vector `ln` will not agree
  bitwise with `libm`'s, so this is another bump; `student_t4`'s expression
  is fixed in the code so the transform can be matched or knowingly
  re-blessed.
- A cheaper main-stream generator: landed as priority 4b below, for the
  dev-build cost rather than the release one.
- Parallel size decoration for `realism`.
- The cascade's fixed-draw core: landed as priority 4c below.

## Priority 4b. One counter-based generator for every tape stream (landed)

Landed 2026-10-17 as tape protocol 38
and `ARRIVAL_KERNEL_VERSION` 4. Sparred with codex to consensus, audited by a
fresh session afterwards.

Why. Measured in a dev build: 24M `ChaCha12Rng` words took 7.3 s, about
300 ns per word, and 1M legacy parents through `advance_parent` 4.15 s. At
twelve main-stream words per parent the cipher was about 3.6 of the 4.15
microseconds, and that is what kept `realism` and
`session_modulation_reproduces_curves` past the 20 s watchdog. `opt-level`
is ruled out, so the generator had to get cheaper.

What it is now. `mogwai_data::TapeRng` (`crates/mogwai-data/src/tape_rng.rs`):
Philox4x32-10, the published Random123 constants, pinned by their
known-answer vectors. Key: `splitmix64(river seed)`. Counter: block in the
low half, a stream domain in the high half, so one river's streams sit at
disjoint counter addresses. One `u64` per position, two per block, the last
block cached by address. Every generated stream moved to it: main, cadence,
size (position `2 * child`), step (position = child index), book, reporting,
and the lab envelope simulator. The session-segment composer and the
engine's fill-band ChaCha8 did not move.

SplitMix64 in counter mode was proposed first and refused in the spar: every
key is a rotation of one sequence, and a slot read at stride 12 is SplitMix
with gamma `12 * gamma`, outside its published evidence.

Re-blessed: the five arrival transcripts (`regenerate_arrival_transcript` now
regenerates all five from their own headers), `clean_regime_is_byte_identical`,
the size-stream head, the cascade hashes, the stage-A manifest (version
fields and digest only, no cell moved), the fill golden and the tape oracle.

Measured, dev, one test alone:

| Test | Before | After |
|---|---|---|
| `realism` | 24.0 s | 8.4 s |
| `session_modulation_reproduces_curves` | 65.1 s | 17.0 s |
| `session_edge_spike_lifts_realized_clamp` | 9.0 s | 2.4 s |
| `parent_helpers_match_the_record_readers` | not recorded | 3.4 s |

Statistical validation. The realism statistics were surveyed over 64 seeds,
against a ChaCha-backed arm of the same code. Seeds 32 to 63 agree to the
third decimal (lag-10 abs ACF 0.1646 against 0.1645, lag-50 0.1135 against
0.1139). Seeds 0 to 31 read higher on Philox (0.1758) because a few
realizations land extreme. A main-stream-only ChaCha arm reproduced the
all-ChaCha arm seed for seed, so each seed's statistic is decided by its
main-stream realization, and the per-seed distribution is heavy-tailed. The
t(4) innovation tails and |t| autocorrelations of the two generators agree
within noise over 20M parents. The GARCH rail-sizing tail was compared the
same way over 64 seeds: mean RMS 1.1736e-5 against 1.1745e-5, per-seed sigma
maxima from about 7x to 65x against 7x to 57x. The eight-seed sizing run
reads 21.3x where ChaCha read 57.2x, one draw of that distribution;
`consts.rs` now says so.

Consequences:

- `realism` lost its `debug-slow` ignore, whose reason was now false, and
  runs in every lane. It is red at seed 42 on `abs_return_acf_lag10`
  (0.186), the long-known fit miss, now filed in `notes/todo.md`. So the plain
  `brokkr check` is red on that one test until the fit moves.
- `session_modulation_reproduces_curves` keeps its cost ignore, with the
  dev wall corrected to about 17 s.
- `opt-level = 3` was deleted from `[profile.release]`: it was the default,
  so the build is unchanged, and the line contradicted the owner ruling.

Found on the way, open:

- The rail-sizing harness and the second-moment harness still draw the old
  ziggurat-plus-gamma t(4) through `draw_student_t`, not the fixed-word
  `student_t4` the bounce walks ship. Same law, different sampler; worth
  aligning when that harness is next touched.

## Priority 4c. The cascade's fixed-draw core (landed)

Landed 2026-10-17 as tape protocol 39.
Designed in a codex spar, two rounds to consensus, and audited by a fresh
session afterwards; the audit found no production defect and four test and
comment gaps, all closed.

What it is now. The cascade reads no main-stream word. Its draws live in
seven `TapeRng` domains: init, component step, window, jump, immigrant,
offspring and parent. Each unit reads a fixed number of words from its own
domain, so its position is an allocation count:

- a component step reads `K + 1 + 2L` words, on every one-second step and
  every closure crossing;
- an evaluated window reads two, empty or not, jumps declared or not;
- a jump reads two;
- an immigrant reads one, per staging, not per parent;
- an offspring reads one, dropped ones included;
- a parent reads a fixed packet, every word drawn whatever branch runs: one
  count per kernel, the t words, the gap pair, the repeat coin, the slot,
  the renewal side and Pareto, and the sweep's branch and geometric words.

Every transform is rejection-free (`draws.rs`):

- normals are the AS 241 quantile;
- Poisson counts invert one uniform: from zero below a rate of 30, outward
  from the mode above it, using Loader's saddle-point pmf;
- the old `min(Poisson, 1e6)` law is kept, with a shortcut to the ceiling
  where a Chernoff bound puts the rest below `1e-30`, subnormal tails
  retired, and a mode residual rule;
- the t takes an integer `student_df` in 3 to 64. That is a contract
  change: a fractional value is now refused. MNQ ships 4.

The cascade's level steps moved to the step stream, and the discrete-book
path leaves the sweep words unread. Two contracts were settled in the spar.
An armed halt still does not age the components; that is now stated at
`reset_bucket` and pinned. Counters are allocations, never rewound.

Pins, each bite-checked by a text-edit mutation:

- `every_unit_allocates_its_fixed_stride`: words against units counted at
  the call sites, at df 4 and 5, jumps on and off;
- `a_clone_mid_walk_continues_exactly`;
- `a_reset_spends_and_rewinds_nothing`;
- `student_df_is_an_integer_in_its_range`;
- `poisson_inversion_partitions_the_unit_interval_by_the_law`: grid
  partition against an independent pmf, both search orders, the threshold,
  and the ceiling fold;
- `poisson_inversion_handles_its_edges`;
- `integer_t_has_its_variance`: variance and symmetry, df 9 and 10.

Every cascade statistical test passes on the new draws: the mean rate,
closures, sign memory, propagator and gap tests in `cascade.rs`, and the
MNQ preset and lab tests in the plain lane. The bounce walks are
byte-identical: the BTCUSDT oracle and the fill golden did not move.
Re-blessed: the cascade hashes and the stage-A manifest.

Measured, `advance_parent` per million MNQ cascade parents, one binary
each, tape 38 against tape 39:

| Build | Tape 38 | Tape 39 |
|---|---|---|
| release | 350 ms | 364 ms |
| dev | 1.68 s | 1.86 s |

Break-even to slightly slower, as tape 37 was for the bounce walks. The
fixed packet draws words a parent does not use, and the gain is in the
batching the fixed addresses now permit, not taken here by owner
instruction. The near-watchdog `fills.rs` and boatyard tests measured
about 1.3 s each in dev on the new tree, so that stale list has cleared.

Found on the way, open:

- `arrival_screen_layer1_reproduces_the_committed_12a_generated_blocks`
  (gate lane) fails with a `projected_child_inside_closed_halt_segment`
  screen refusal at seed 1. It fails identically at tape 37, 38 and 39, so
  it predates this work. It compares against `analysis/mnq-measure-12a.json`,
  last regenerated 2026-08-06. Filed in `notes/todo.md`.

The reports' original framing, kept for the open items:

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

## Priority 4d. SIMD: batched Philox in AVX2 (landed), vector transforms (rejected as built)

Landed 2026-10-17 as tape protocol 40,
a bump taken because the generation path changed, not because a byte did:
every golden, hash and transcript stands. A fresh codex audit found no
unsafe-code defect and no word mismatch. It did find the missing bump, an
over-promising comment, a size test that did not reach `decorate`, and an
exhaustion test that did not check untouched destinations; all four are
closed. Decoration is now checked from inside `decorate`, test builds only,
against one per-child draw for every trade, so every sized walk in the
suite pins it. Sparred with
codex in three rounds, the third on measurements. Two fresh subagents wrote
the first drafts, one per file: the portable Philox lane kernel with the
exact fill, and the vector math kernels.

What it is now.

- `TapeRng::fill` writes the next words of a stream into a caller's slice,
  bit-identical to the same number of `next_word` calls in every pattern:
  an odd start, a carry across the low counter word, interleaved seeks and
  clones, and exhaustion. Batches of four blocks go through a lane kernel;
  a tail of more than one block still runs a whole batch and keeps the words
  it needs. No buffer was added to `TapeRng`, whose size is unchanged.
- On x86-64 with AVX2, chosen at run time, the batch is
  `philox4x32_10_avx2`, written with `std::arch` intrinsics. Each 64-bit
  lane holds one block, so each round is two `_mm256_mul_epu32`, and the
  state stays in registers for all ten rounds. Elsewhere it is the portable
  structure-of-arrays kernel. Both are integer arithmetic and bit-identical,
  so the choice cannot touch a tape and needs no build configuration;
  consumers such as broadarrow get it too.
- Consumers: the legacy packet (12 words), the integrated packet (7, still
  drawn only after the arrival kernel succeeds), the cascade parent packet,
  and size decoration, in fills of up to 32 children.

Pins:

- the portable kernel and the AVX2 kernel against the scalar block on
  every lane, across the low-word carry and the top of the block range;
- `fill` against `next_word` for every start and length, plus interleaved
  fills, seeks and clones, and exhaustion;
- `batched_size_words_are_the_per_child_draws`, which covers chunk
  boundaries and deep starts.

Each was bite-checked by a text-edit mutation.

Measured, release, this host:

| | Before | After |
|---|---|---|
| 120M words, `next_word` against `fill` in runs of 64 | 546 ms | 255 ms |
| legacy, per million parents | 214 ms | 185 ms |
| integrated, per million parents | 231 to 242 ms | 220 ms |
| cascade, per million parents | 355 to 377 ms | 317 ms |
| `realism`, sized, 2M parents | 1.73 s | 1.52 s |

Rejected, recorded so it is not retried as built:

- Autovectorization. The portable kernel, compiled under
  `target_feature(avx2)`, measured slower than its baseline build (442
  against 393 ms per 120M words). The intrinsics kernel is what delivered.
- Vector transforms with every branch evaluated per lane. In-tree ln, exp
  and the AS 241 quantile, accurate and monotone, with every branch
  evaluated per lane and selected afterwards: lognormal sizes 467 ms against
  244 ms scalar per 16M, normals 343 against 121. They tripled the work to
  expose two baseline lanes. The module was deleted.

Open:

- The central-branch-only vector quantile: landed as priority 4e below.
- Cross-parent batching of innovations. It needs scratch owned by the
  consumer and a position that reports what was consumed, not what was
  computed.

### SIMD background for the next session

Hardware, by owner statement (2026-10-17): the target machines are AMD
Zen 4 or later. So AVX2 and AVX-512 are both present. On Zen, AVX-512 has
no frequency penalty: Zen 4 maps 512-bit operations onto 256-bit units, and
Zen 5 has native 512-bit hardware. An AVX-512 path is a reasonable next
width after the AVX2 kernel, chosen at run time the same way. Scatter and
gather are slow on Zen; avoid them.

From "The state of SIMD in Rust in 2026" (Sergey Davidoff, 2026-09-25;
the owner verified its claims):

- Algebraic float operations (`algebraic_add` and kin) are stable since
  Rust 1.98. They let the compiler reassociate, which float
  autovectorization needs, but code usually has to be rewritten to use
  them.
- Autovectorization is unreliable: it needs small fixed-width loops
  (`as_chunks`), and the result must be checked by measurement or
  assembly. This report confirms that: the portable Philox kernel under
  AVX2 was slower than its baseline build.
- `std::simd` is nightly only, with occasional breaking changes. Its
  `sin` and similar functions are scalar underneath, so its non-trivial
  float functions are not worth using.
- `fearless_simd`: version 1.0, with multiversioning through `#[simd]`
  annotations and AVX-512 enabled only where it does not hurt. It has no
  transcendental functions, so ln and exp would still be ours to write.
- `wide`: has transcendentals, but their precision is unspecified, and it
  has no multiversioning.
- `pulp`: has runtime dispatch, and is the only crate with safe access to
  intrinsics outside its predefined levels, though how is undocumented. The
  whole hot loop must run inside one `vectorize` context, or the per-call
  overhead dominates.
- `macerator`: like `pulp`, generic over element type, with little
  documentation.
- `multiversion`: has a small per-call cost, so it belongs on functions
  with loops, with `#[inline(always)]` below it.
- Since Rust 1.87, intrinsics are safe to call inside a
  `#[target_feature]` function. Loads, stores and the call from outside
  remain unsafe; this tree's AVX2 kernel already works that way.
- 32x32 to 64-bit lane multiplies exist on SSE2, AVX2, AVX-512 and NEON.
- Float arithmetic on denormals is slow: about 30 times on Intel and 2
  times on AMD. Keep the existing safeguards against subnormals, such as
  the Poisson search's tail retirement.

The next item, as designed with codex, is a central-branch-only vector
quantile:

- classify each lane by branch;
- compute the central AS 241 ratio, about 85 percent of inputs, in vector
  lanes;
- compute the tail lanes with the existing scalar code, lane by lane,
  never by falling back for the whole batch;
- keep the central results bit-identical between the vector body and any
  remainder, either through a fixed sequence of operations with no fused
  or width-dependent reassociation, or by running remainders through
  padded vector calls, so a value never depends on batch length, reader or
  checkpoint placement.

## Priority 4e. A bit-identical vector quantile and the size law's own exp (landed)

Landed 2026-10-17 as tape protocol 41, sparred with codex to consensus in
two rounds.

What it is now. `generated/vecmath.rs`:

- `normal_quantiles`: four AVX2 lanes compute AS 241's central branch in
  the scalar expression tree exactly, `(q * num) / den`, separately
  rounded, no FMA; the uniform is built in scalar code, because AVX2 has no
  exact unsigned 64-bit to double conversion; tail lanes and remainders call
  the scalar `normal_quantile`. The result is bit-identical to the scalar
  quantile, so it cannot depend on batch length or position.
- `tape_exp` and `tape_exps`: a Cody-Waite reduction (fdlibm's 32-bit
  `LN2_HI`), rounding to nearest even through the `1.5 * 2^52` shifter in
  both paths, a degree-13 Taylor Horner, and `2^n` built from exponent bits,
  over a refused-outside domain of -700 to 700. The scalar and the vector
  code share one expression tree, so they also give the same bits. It is
  not `libm`, so the bump moves sizes; `SizeStream::new` refuses a size law
  whose two extreme words reach outside the domain.
- The vector path runs only in builds without debug assertions. Dev builds
  run scalar, and the tests call the AVX2 kernels directly. In dev, every
  intrinsic is a call.
- Consumers: size decoration (`SizeStream::draws_from`, one batch per
  32-child fill) and the cascade's component step (one fill of K + 1 + 2L
  words, applied in the step's order: texture, fast, level, sigma extra).

Pins, each bite-checked by a text-edit mutation:
`batched_quantiles_are_the_scalar_bits_at_every_offset_and_length` (words
bracketing both cutoffs and the midpoint, the grid's ends, every offset and
remainder; `q * (num / den)` fails it by one ulp);
`batched_exps_are_the_scalar_bits_at_every_offset_and_length` (every
reduction transition and half-transition, plus or minus three ulp; a
reassociated reduction fails it); `tape_exp_stays_within_two_ulp_of_the_platform_exp`
(a differential gate only: Rust leaves `f64::exp`'s precision unspecified);
`tape_exp_meets_its_reference_values`; domain refusals for NaN and past 700.
`clean_regime_is_byte_identical` sees the exp: a 1e-6 relative error moves
its sizes.

No committed golden moved. The exp is within an ulp or two of `libm`, and
a size crosses a quantization or snap threshold on that error with
negligible probability. The cascade hashes, the oracle and the fill golden
re-ran unchanged. The stage-A manifest re-blessed its version and digest.

Measured, release, 16M sizes in 32-child chunks (`size_transform_walls`,
ignored, run with `--release`): tape 40's chain (scalar quantile, `libm`
exp) 271 ms; scalar quantile with `tape_exp` 334 ms; batched 168 ms. That is
about 6 ns per child, roughly 50 ns per legacy sized parent. The
`realism` wall did not show it at the time, because test builds then re-ran
every size through a scalar per-child check inside `decorate` (moved out by
priority 4g).

Found on the way: `realism` took 18 s alone in dev at tape 40 (this host,
2026-10-17), not the 8.4 s priority 4b recorded. Under `brokkr check` load it
timed out and took the rest of the `mogwai-data` binary down with it.
Priority 4g fixed that.

## Priority 4f. The bounce packets' random-input logarithms (landed)

Landed 2026-10-17 as tape protocol 42, sparred with codex to consensus. The
cross-parent prepared-packet cache was designed and dropped in the spar.
The win it was for (vector logs) fits inside one parent, so no position,
clone or checkpoint state is needed.

What it is now.

- `tape_ln` (`vecmath.rs`): fdlibm's `e_log` tree applied uniformly,
  without its special branches, refusing anything but a positive normal.
  The scalar and AVX2 code share one expression tree.
  `tape_ln(1.0)` is exactly zero.
- `LegacyLogs::of`: the legacy packet's three random-input logs (the gap's
  open uniform, the t(4) denominator's rounded product, the count
  geometric's) in one `tape_ln4` call, the fourth lane idle at one. They are
  applied where they always were: `next_duration_ns` takes the gap's log,
  `next_latent_mid` the numerator word and the chi log, and
  `mixture_count_from_log` the geometric's. The integrated packet's one
  log is a scalar `tape_ln`.
- The cascade keeps the platform `ln` through `mixture_count` and
  `unit_exponential`, so one law has two log implementations on different
  walks, by design. The cascade hashes are the scope check and did not move.
- `SweepShape::new`'s `ln(1 - 1 / m)` stays on the platform `ln`. Taking it
  into the fourth lane would mean resolving the state-dependent shape
  before the call.

Pins, each bite-checked by a text-edit mutation:
`four_lane_logs_are_the_scalar_bits_in_every_lane` (powers of two and
`sqrt(2)` cutoff neighbours in every reachable binade, values just below
one, `2^-53` and `2^-106`, every lane rotation); `tape_ln_stays_within_one_ulp_and_keeps_its_sign`
(differential against the platform `ln`, negative below one, monotone over
the sorted probes); `tape_ln_meets_its_reference_values`;
`legacy_logs_take_each_slot_to_its_own_lane` (a lane swap in `of` fails
it); `clean_regime_is_byte_identical` sees the log (a 1e-6 error moves its
timestamps).

No committed golden moved, for the same reason as 4e: the log is within an
ulp of `libm` and the walk rounds to nanoseconds and ticks. `realism`'s
lag-10 statistic is the same to the last bit. The stage-A manifest
re-blessed.

Measured, release, 16M packets (`packet_log_walls`, ignored): three
platform `ln` 225 ms, three scalar `tape_ln` 296 ms, `tape_ln4` 176 ms.
About 3 ns per legacy parent, under 2 percent. Small: a `target_feature`
function cannot inline into its caller, and three lanes of four is the
whole batch. Cross-parent batching is what would widen it, now without a
log question attached.

## Priority 4g. `realism` off the watchdog (landed)

Landed 2026-10-17, sparred with codex. Exact: no tape byte moves.

- The per-child decoration check left `decorate`, where it ran on every
  sized test walk and repeated about seventeen million scalar size draws
  in `realism` alone. It is now `decoration_is_one_draw_per_trade`: walks
  with a children mean of 40 so plans cross one and two 32-child chunks,
  both size grids, sized, record-wise and unsized readers in turn, a clone
  decorated apart, deep child indices, both snap branches, each asserted
  reached. Bite-checked on a wrong seek and on a stale second chunk.
- `realism`'s `measure` walks unsized. Every child of a placed parent is
  one trade (asserted per parent), so the size moments are taken over the
  walk's child-index range on worker threads, by `child_quantities` (the
  production fill, batch transform and quantizer), in blocks of 65,536
  children folded in order. `measure_records` blocks its trade-by-trade
  sums the same way, so `compact_measurement_matches_the_record_reader`
  still holds bit for bit. Reversing the fold fails it on `size_cv`.

Measured, dev, alone: 16.8 s to 10.2 s (the unsized walk is about 6 s of
it). Under `brokkr check` load it now reaches its verdict instead of the
watchdog, so the plain check is red only on the known
`abs_return_acf_lag10` fit miss.

## Priority 4h. Size decoration across parents (landed)

Landed 2026-10-17 as tape protocol 43, sparred with codex to consensus. No
byte moves; the bump is taken for the path change, as at 40.

Why. Release, 2M legacy parents: the unsized walk took 350 ms and the sized
walk 834 ms. With the size work stubbed out of `decorate` the sized walk
took 410 ms, so sizes cost about 25 ns per child, against about 10 ns for
the bare batched transform. The rest was per-parent overhead at about 8.5
children per call.

What it is now. `QuantityCache` (`source.rs`): final quantity mantissas
for 256 consecutive child indices, computed by the decoration's own fill,
`draws_from` and quantizer (one 512-word fill, eight 32-child batches).
`decorate` takes each trade's quantity from it, and refills from the first
child still owed one on a miss, so a plan longer than a block refills as it
goes. Membership is by actual index, so holes left by skipped or unsized
parents are fine. A quantity depends on nothing but the child index, the
seed's size stream and the quantizer: `with_surge`, the one `make_mut`,
changes neither. A clone carries no block (`Option<Box<_>>`, cleared on
clone), so checkpoint snapshots pay nothing; checkpoint extension never
decorates. A refill is published only once it is complete. It clamps
against the last readable child (`2^63 - 2`), and when not even the next
child is readable the old per-plan path runs and refuses as before.

Pins, each bite-checked: `a_cached_quantity_is_its_childs_own_draw` (an
oracle of one `draw` per index: hits, two refills, a jump back, a hole
forward, a clone starting empty, the last readable children, `None` past
them; a block recording the wrong first child fails it);
`decoration_is_one_draw_per_trade` now also asserts that a plan crossed a
whole block.

Measured, release, 2M legacy parents (`sized_walk_walls`, ignored): sized
walk 834 ms to 745 ms, quantity checksum identical. That is about 20 ns per
child against 25. What is left is mostly the per-trade lookup and
quantizer, plus the transform itself.

## Priority 4i. Where a legacy parent's time goes, and Philox chains (landed)

Measured 2026-10-17 with a temporary TSC probe around each stage of
`begin_legacy_event` (release, 2M parents, about 25 cycles of probe overhead
per mark). The probe was removed afterwards.

| Stage | Cycles per parent |
|---|---|
| sweep emission (step gaps, resolve) | about 161 |
| main-stream words and logs | about 140 |
| latent mid (GARCH, quantile, exp) | about 114 |
| count | about 48 |
| repeat and quote | about 39 |
| gap and clock | about 37 |
| side and book | about 37 |

`hotpath` was not usable for this: it queues one event per instrumented
call and drains at about 1.3M per second, which would swamp stages of a
few tens of nanoseconds.

Landed as tape protocol 44, audited by a fresh codex session (no defect;
its coverage gaps closed). The AVX2 Philox kernel was one four-block chain
bound by the multiply's latency, ten dependent rounds deep, at about 8
cycles per word. It now runs up to four independent chains interleaved
(`philox4x32_10_avx2::<N>`, N of 4, 8, 12 or 16). `fill_range` runs full
16-block batches and covers a tail with the fewest chains that reach it.
Every word is unchanged: integer arithmetic, and the pins compare every
lane of every width against the scalar block, `fill` against `next_word`
for every tail width and batch boundary across the low-word carry, and
exhaustion at every tail width, including a 16-block tail ending exactly at
the top.

Measured, release (`fill_walls`, ignored), 120M words, one chain against
interleaved: runs of 12 took 371 ms against 273, runs of 17 331 ms against
244, runs of 64 284 ms against 220. The whole walk moved little: unsized
350 to about 340 ms per 2M parents, sized 745 to about 730. The fill is not
the share it looked like, and the step gaps still read scalar blocks.

AVX-512 was not built: this host reports `avx512f` false, so a kernel could
not be tested here. The owner states the targets are Zen 4 or later; an
8-lane kernel belongs on a host that can run its pins. `bygg`, another host
we work on, is Zen 5 (native 512-bit units): that is where to build and pin
it.

## Priority 5. Rail sizing as independent seeds (landed)

Landed 2026-10-16, sparred with a fresh
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

Closed 2026-10-17. Report B's per-parent Poisson construction in
`spawn_children` was gone already: priority 4c's `poisson_inverse` builds
nothing. Its small re-derivations are all done: `is_round_lot`'s lot and
`owed_children` went with priority 2, and `LevelSteps` now takes a
`StepLaw` whose `ln_1p(-p)` is built once per source in `ChildCoins`, which
is exact.

1. Per-parent `Arc` clone (report B). `Arc::clone(&self.spec)` in each
   `begin_*` is an atomic increment and decrement per parent, there only for
   the borrow checker. Split the borrows.
2. Size stream (report B). Each child uses two u64; splitting one between the
   size uniform and the snap coin halves it. Tape bump. (The counter-based
   generator half of this landed as priority 4b.)
3. Cascade immigrant placement (report B). Each immigrant did a `powf`;
   drawing a second's immigrants at once avoids it. Tape bump. Not re-read
   since priority 4c rewrote the immigrant draw; check the site first.
4. Lab calendar lookups (report B). `open_throughout` scans the calendar's
   windows per call; the generator's `CompiledCalendar` minute table
   (crate-private today) could serve the lab.
5. A specialized Student-t sampler (report A), after work elimination and
   block execution. Payoff depends on sampler and hardware, and the cascade's
   degrees of freedom are configurable, so a hardcoded legacy sampler must not
   quietly become the cascade's. Priority 4's fixed-draw t(4) is report B's
   form of the same idea.

## Afterwards, if these work

- One parent-block producer feeding sinks as the runner for every lab walk:
  `arrival_screen`, `arrival_envelope`, the fit driver and `gen --type summary`
  (report B).
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

To time a test past the 20 s per-test watchdog, raise it and ask for the
wall: `brokkr test -p <crate> <NAME> --timings --timeout 280`. `--timeout`
is the per-test watchdog in seconds; `--timings` prints every matched
test's wall, slowest first. Without them an over-watchdog test reports only
"exceeded the 20s per-test timeout", not how long it would have taken.

At tape 37, measured that way: `realism` 24.0 s, and it passes at seed 42;
`session_modulation_reproduces_curves` 65.1 s.

None is left on this workload. `realism` lost its ignore at priority 4b
and runs in every lane, at about 10 s alone in dev since priority 4g.

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

- The shared-spec and compiled-calendar change (before tape 36) was never
  benchmarked on the calendar presets' seeks, history and checkpoints.
