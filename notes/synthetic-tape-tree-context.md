# Synthetic tape v2: what the mogwai tree already knows

Written 2026-09-02. Companion to `synthetic-tape.md`, which states the v2
programme. This document records what the mogwai tree already contains that
binds, corrects or unblocks that programme. Audience is agents working v2.

Sources read, and nothing else in the tree: `reference/glossary.md`,
`reference/north-star.md`, `reference/architecture-generator.md`,
`notes/tape-research-v1.md`, `notes/segment-sampler.md`, `notes/todo.md`,
and the source of `crates/mogwai-data` and `crates/mogwai-lab` (module
headers, public types, the generator's constants and step functions; not
every line of the 57,000).

Where this document and `synthetic-tape.md` disagree, the programme document
is the one to amend, and section 9 lists the amendments owed.

---

## 1. The v1 arc, and the rules that come out of it

A measure-and-fit arc ran from about 2026-08-01 to 2026-08-12: over a hundred
commits, `TAPE_PROTOCOL_VERSION` driven from 1 into the twenties, five fitted
arrival-mechanism families screened across 1,402 parameter cells. It ended
with a tape the owner judged indistinguishable from the one it started with.
The owner closed it on 2026-08-23. `notes/tape-research-v1.md` is the record.

The diagnosis, which v2 inherits as rules:

- **The objective was never a statistic.** Every gate measured a projection
  at one horizon. The defects the eye found on a rendered chart were not
  among the things any gate measured: the cash open did not ignite, there
  were no reopen gaps, volume looked uniform across sessions. The proxy was
  orthogonal to the largest defects.
- **Preregistration is for certifying a candidate, never for finding one.**
  Freezing a search space before the target geometry is known buys one bit
  per cycle at a cost of weeks. Twenty mechanisms passed three of four gates;
  mechanism supply was never the bottleneck, agreement on the criterion was.
- **The apparatus ate the effort.** Three of four hard gates in the first
  screen were defective in the flattering direction. Roughly 25,000 lines of
  gate machinery are still compiled into the tree (section 7).
- **Confirmation was never affordable.** July was spent as design evidence
  seven times over, and a one-month confirmation with adequate power may not
  exist because between-month regime spread dominates the effect.
- **The cheapest gate that can see the objective goes first.** Here that is a
  rendered chart under the owner's eye, via `analysis/plot_tape.py`.
- Every demand for measurement names the decision the result would change.
  This is a standing rule in the tree's `CLAUDE.md` process reset, not only
  a v1 lesson.

Consequence for the programme: `synthetic-tape.md` puts the chart gate at the
end of the validation battery. It belongs at the front, as experiment zero.

---

## 2. The bar

The one positive empirical result of the whole arc is the owner's acceptance,
on 2026-08-18, of the texture of real resampled MNQ Asia segments: movement,
consolidations, ranges and breaks all read as real on a quiet stretch. The
same eye rejected five fitted mechanism families and the shipped generator at
two commits 201 apart that looked the same.

That is the bar a fitted v2 engine has to clear, stated plainly. It is not a
statistic and cannot be pre-passed by one.

The owner's defect list from that comparison, which is the first eyeball
criteria set:

1. The cash open must ignite at 09:30 New York, at minute scale. The shipped
   hourly profile smears it across the hour.
2. Reopen gaps across the daily close must exist. Real Asia opens with gaps
   up to about 300 points on MNQ at a 20,000 level (largest measured
   -1.31 percent). The clean generated tape has none.
3. Volume must visibly vary across sessions.
4. Macro-release spikes are low priority to the eye.
5. State times in UTC and say what they are in the viewer's frame. The owner
   is in Norway.
6. A big move has a duration. Added 2026-09-06 from the owner's reading of
   the protocol 31 week: MNQ moving 300 to 400 points in the New York open
   is normal, over seven to twenty minutes; the same move inside two minutes
   is not, at the open or anywhere else. The real week of 2026-08-16 agrees:
   its largest two-minute move was 113 points and its largest twenty-minute
   move 293, both at the cash open. The shipped walk produced a 200-point
   move in two minutes on a Wednesday afternoon and three to four times as
   many 200-point twenty-minute moves as real. The engine's extreme must
   build and decay over minutes, never compound inside one.
7. Overnight volume is quiet with bursts that taper, not a wall. Added
   2026-09-06 from the owner's reading of the same week: real Asia and
   London volume runs about half the protocol 31 tape's in general, with
   spikes up to the tape's general level that then taper off. Measured on
   the real week: the overnight p10 minute is about half ours, the p99 is
   above ours, one real minute in six is a burst at twice the phase median
   against one in fifteen to twenty-five on ours, and real minute-volume
   autocorrelation starts near 0.6 and decays over fifteen minutes. This is
   the v1 "clustering at the wrong scale" finding seen by eye: the
   envelope is the deterministic rate, and the bursts are the slow kernel
   component of L2 with the level belonging to L1.
8. A big minute is sized by its hour and comes with volume. Added
   2026-09-06 from the owner's reading of the first cascade chart, a
   244-point drop in one Asia minute on ordinary volume. In 258 real
   sessions Asia's largest minute is 140 points and only four exceed 100;
   the largest minute per phase runs 140 to 390 across a threefold spread
   in minute sd, so news size follows the hour weakly; and every real
   minute over 100 points carries two to ten times the phase's median
   volume. A silent jump, or a night jump the size of an open one, fails
   the eye at once.
9. A night swells over hours. Added the same day: the owner read the
   overnight volume as flat even with the minute texture right. The
   whole-session volume autocorrelation at one, two and four hours is
   0.18, 0.11 and 0.03 on real; a texture with nothing slower than ninety
   minutes reads 0.07 and 0.04.

The segment-sampler gate itself failed on 2026-08-18 for a different reason:
300-point moves inside the Asia session body, carried in from the cut data
rather than produced by composition, plus a seam level discontinuity that
contaminated the gaps-off control. Both are recorded in
`notes/segment-sampler.md` and the todo, and neither is a v2 fitted-engine
concern beyond one lesson: a rare impossibility still dominates the eye on a
chart, so a heavy tail that is right on average and wrong at the extreme
fails.

---

## 3. Reconciliations owed by the owner

These are places where the tree's authoritative text and the owner's
2026-09-02 decisions differ. Agents do not resolve them; they are listed so
nobody re-derives them.

- **Direction.** `notes/segment-sampler.md` carries the standing direction as
  resampled real segments, and `north-star.md` says session footprints are
  "built by resampling real segments". The owner told v2 on 2026-09-02 that
  the tape is a fitted generative process. The todo says v2 has not started.
  The north star sentence and the note are the owner's to edit.
- **Reopen gaps.** `architecture-generator.md` states as a structural fidelity
  limit that the clean baseline river never produces a reopen jump, and that
  `ReopenGap` "remains havoc and remains unscheduled". The owner has ruled
  that the v2 futures tape carries the ordinary jump at every scheduled
  reopen, with the `ReopenGap` arm as an unscheduled extra. The architecture
  sentence describes the present build and will be false once v2 lands; the
  glossary entry for `ReopenGap` may want one clarifying sentence.
- **Session classes.** The north star names three classes and about five
  presets. The owner added forex on 2026-09-02, and the glossary already
  carries the `forex` instrument class, so this is an extension rather than a
  conflict. The nautilus adapter cannot carry a forex instrument today
  (todo, Adapter section); the venue and protocol can.

---

## 4. The shipped generator

`crates/mogwai-data/src/generated/`, the `GeneratedSource` lineage. Fitted to
Kraken crypto trade history (eight pairs, 298 million trades, anchored on
XBTUSD) with an MNQ overlay from one July 2026 TBBO month. What v2 replaces,
and what it must interoperate with.

Since 2026-09-06 (tape protocol 32) the MNQ preset no longer runs the walk
described in 4.1: it runs the activity cascade, a second walk inside the
same struct selected by `[instrument.generator.cascade]`, whose mechanism
and evidence are in `mogwai_data::generated::cascade` and
`notes/synthetic-tape-e0.md`. Sections 4.1 to 4.3 describe the walk every
calendar-less preset still runs, and the sweep, size, book, calendar,
envelope, checkpoint and seed machinery the cascade reuses unchanged.

### 4.1 Mechanism

- **Parent and child sweep.** One parent match event updates the latent
  market once and emits a same-side sweep of one or more child prints,
  `INTRA_EVENT_STEP_NS = 1_000` apart (one microsecond), walking
  monotonically in the take direction. Child count is a mixture: with
  probability `q` exactly one, else geometric with mean `m`, both solved in
  closed form from `children_mean` and `children_single_frac`. Capped at
  `CHILD_CAP = 4_096`.
- **Arrival clock (shipped path).** A two-state Markov-modulated Weibull
  duration clock indexed by event: quiet fraction 0.35, state persistence
  0.90 per event, quiet-to-active mean ratio 150. Sweep size is conditioned
  on the state the gap was drawn from. `ARRIVAL_MEAN_CAL = 0.944` corrects
  this scheme's realized-mean inflation.
- **Arrival kernels (integrated path, protocol 12b).** Four families exist in
  `arrival.rs`: `WallMmpp` (occupancy, rate ratio, tau), `LogOuCox`
  (sigma_y, tau), `SelfExciting` (phi, tau), `ShotNoise` (m, k, tau). All
  integrate a baseline over the calendar in one-second steps. No shipped
  preset declares one; `TAPE_PROTOCOL_VERSION` 15 is reserved for a landing
  that never happened.
- **Latent mid.** GARCH(1,1) with `a1 = 0.02`, `b1 = 0.979`, Student-t
  innovations with 4 degrees of freedom standardized to unit variance, driven
  once per parent. Three rails: a GARCH state cap (`GARCH_SIGMA_CAP = 1e-3`),
  a feedback-return ceiling (`4e-3`), and an absolute realized-return ceiling
  (`5e-2` per event, about +5.13 / -4.88 percent). The first two are never
  scaled by a regime; the third is an output envelope.
- **Side and drift.** A bounce state with high and low regimes and a
  same-direction drift, all module constants (`BOUNCE_*`, `DRIFT_*`,
  `HOT_DRIFT_PROB`), plus `EVENT_PRICE_REPEAT_PROB = 0.8`.
- **Session.** `SessionProfile { intensity_hour: [f64; 24], vol_hour: [f64;
  24], dow_weight: [f64; 7] }` applied as multipliers, normalized over
  calendar-open minutes when a calendar exists. `SessionCalendar {
  utc_offset_minutes, open_windows: Vec<WeeklyWindow{start_minute,
  end_minute}>, settlement_minute_of_day }` in exchange-local week minutes;
  the generator jumps a closure whole.
- **Book.** One `QuoteTick` per parent, emitted before the first child at the
  parent timestamp. Exact positive integer-tick width centred with one
  rounding on the drifted latent mid; constant top sizes; trades displaced
  half a tick from mid so they print at the touch. Width, top sizes, depth
  levels, depth growth and displacement are separate knobs with
  `CalibrationProvenance { Uncalibrated | Fitted { corpus } }`. Only MNQ has
  a fitted touch (3 by 3); everything else is a placeholder, which is the
  usability cliff the todo records for market orders.
- **Regime overlay.** Per-subscription havoc envelopes (`VolStorm`,
  `SessionEdgeSpike`, drought) reach the mid only through a volatility
  multiplier and meet the same fixed ceiling.

### 4.2 Knobs, as they exist today

`GeneratorScalars`: `symbol`, `modal_tick`, `price_decimals`,
`mean_event_duration_s`, `children_mean`, `children_single_frac`,
`levels_mean`, `size_round_frac`, `start_price`, `latent_size_median`,
`size_log_sigma` (default 1.15), `vol_scalar`, `quoted_width`, `top_sizes`,
`depth_levels`, `depth_growth`, `trade_displacement_ticks`, `arrival`
(optional kernel). Plus `SessionProfile` and `SessionCalendar` on the
instrument. Everything else is a compile-time constant in `consts.rs`.

The fingerprint (`analysis/fingerprint.json`) carries golden stylized-fact
targets with min-median-max bands from the Kraken corpus: duration dispersion,
dwell, return ACF lag 1, absolute-return ACF at lags 1, 10, 50, zero-change
fraction, per-second count moments. Bands diagnose; they never admit or
reject an instrument. That rule stands for v2.

### 4.3 Diagnosed defects, for the record

From `notes/tape-research-v1.md`, mechanism findings that outlive the gates
that found them. Listed so v2 does not rediscover them, and because some are
design lessons rather than bugs.

- **Clustering at the wrong scale, fully diagnosed.** The event-indexed chain
  gives a correlation length of about ten parents. At hundreds to thousands
  of parents per minute, the minute count is a deterministic rate plus
  Poisson noise: Fano near one by construction. Over-clusters at 1 s by up to
  six times, under-clusters at 300 s by up to seventy-seven. The repair is a
  redistribution of clustering across scales, not a slow component bolted on.
  This is the most transferable finding.
- Seeds 1 and 2 produced identical per-minute bin counts at some hours:
  seed-to-seed variation did not reach a bin edge.
- `ARRIVAL_MEAN_CAL` is an artifact of the sampling scheme; on the integrated
  frame it double counts by exactly 1.0593. Gated now.
- The shipped path hardcoded a July UTC offset and degenerated in winter.
  Single-month validation is structurally blind to seasonal frame errors.
- The `children_mean` clamp turned a configured parameter into a constant at
  the MNQ mean of 1.17. Repaired.
- `step_child` advances the clock across the whole child burst, so
  arrival-only simulation is impossible on this generator.
- The minute-range extreme tail is two to four times heavy with zero rail
  hits: per-seed maxima 4,333 ticks against the real month's 968. An
  unconstrained volatility-cluster phenomenon.
- `AutoCorr`'s zero-variance guard misses a positive float residue.
  Deliberately unfixed because the fix moves pinned cadence numbers; v2's
  refit is where it costs nothing.

---

## 5. Market facts v1 established, as v2 priors

All measured on MNQ (July 2026 TBBO, April 2026 cut) unless stated. Each
line says what it binds in `synthetic-tape.md`.

- **Arrival clustering lives in the minutes-to-hour band, not sub-second.**
  Within-session Fano compounds nine to thirty times between 1 s and 300 s.
  Fitted single-exponential tail times run 278 to 3,277 s, median near
  1,000 s. Binds: the kernel mixture in L2 needs a slow component of order
  ten minutes to an hour, and E2's choice of decay rates should span
  milliseconds to that.
- **A stable session-wide common mode in arrival rates**, hourwise
  permutation p = 0.0005, positive one-day dependence, explaining 28 to 30
  percent of residual structure. Binds: L1 needs a day-scale state, which
  MSM's slower components provide.
- **The arrival mechanism must be right-skewed.** High-sigma log-OU cells
  failed by ratios of 47 to over 15,000: they buy the high-count tail at the
  price of silence the tape does not carry. Binds: Hawkes-type excitation is
  right-skewed by construction; a log-OU intensity is retired as an L2
  candidate. L1 as a multiplicative state process is unaffected.
- **Activity-conditioned clustering is refuted.** The clustering shape does
  not move with hour activity. Binds: direct support for E1's parsimonious
  hypothesis, one kernel with per-phase levels.
- **The close hour is a distinct stratum**, and the structure is calendar
  phase and segment position, not a free parameter per UTC hour. Binds: the
  phase taxonomy in L0, including explicit open and close phases.
- **July MNQ is 90.5 percent single-print parents**, `children_mean` 1.1711.
  Binds: sweep multiplicity is a small correction on micros, not a feature;
  fit it, do not design around it. NQ and ES will differ.
- **Per-parent volatility does not track the session volume profile.** The
  per-parent robust scale is nearly flat and slightly inverted, calmer in
  cash hours than overnight; the bar-scale session swing is almost entirely
  arrival density (refitted peak-to-trough 14.5x for arrivals). Binds: the
  per-phase volatility multiplier in L0 should be expected near one, with
  activity carried by intensity, which is what the L3 time change does.
  Caveat from the same note: over ten July sessions volume did not overstate
  arrivals at all (ratio 0.95), so the two results travel together.
- **NQ and MNQ agree on process shape and disagree on level.** Long-memory
  ACFs within 0.008, sweep and return shape within a few percent, MNQ prints
  4.3 times as often. Binds: fit micros separately (as the programme says),
  but share process-shape reasoning across the pair.
- **`zero_change_frac` is derived, not fitted.** It follows from tick size
  relative to per-print movement jointly with arrival rate. Binds: in the L2
  engine it is emergent from the `P±` and `T±` rates, which agrees; it is a
  validation metric, never a knob.
- **Asia is the calmest window**: median one-minute move 1.75 points against
  London 2.25, NY afternoon 3.75, NY morning 5.00. Reopen gaps are
  Asia-specific, across the daily close. Binds: L0's transition table has one
  real gap per day on CME, at the 17:00 CT reopen, plus the weekend.
- **Regression needs one daylight month, one standard month and one
  transition month.** Binds: the intake sequence's phase step and the
  validation battery.
- **Crypto (Kraken, 24/7) intensity swings about 1.8x** between the London
  and New York overlap and the Asian small hours. Binds: the crypto clock is
  one phase with a weak weekly cycle, as the class table says.

---

## 6. The interface a v2 engine implements

From `crates/mogwai-data/src/lib.rs` and the generator.

```
pub trait TickSource {
    fn next_tick(&mut self) -> Option<TickEvent>;
    fn fault(&self) -> Option<TickFault> { None }
    fn seek_to(&mut self, start_ts: u64) -> Option<TickEvent> { ... }
}
pub enum TickEvent { Trade(TradeTick), Quote(QuoteTick) }
TradeTick { symbol: Arc<str>, price: Decimal, size: Decimal,
            aggressor: AggressorSide, ts_event: u64 }
QuoteTick { symbol: Arc<str>, bid_px: Decimal, ask_px: Decimal,
            bid_sz: Decimal, ask_sz: Decimal, ts_event: u64 }
```

Obligations that come with it:

- **Pure function of state.** The source is `Clone`, and `CheckpointIndex`
  snapshots it every K ticks and replays the residual for seeks. Every field
  must be `Clone`, including the RNG (`ChaCha12Rng`, held directly because
  `StdRng` dropped `Clone`). A composed river without a checkpoint chain is
  fenced out of the venue by a guard test; a v2 engine gets the chain for
  free if it is a small `Clone` state, which the programme's design is.
- **Seeds.** One 64-bit run seed; every stream derives by domain-separated
  `splitmix64` with a stream tag; a river's tape root is keyed by the
  requested symbol label. `mogwai_protocol::seeds` owns the derivation.
- **Sweep convention.** BBO first at the parent timestamp, then children one
  microsecond apart walking monotonically in the take direction, sharing the
  parent's book. The venue's fill logic and the fill golden assume this.
  v2 may change the spacing (it is a fitted quantity in the programme), but
  the ordering convention is what consumers read.
- **Closures.** The calendar owns hard closure; the generator jumps a closure
  whole and never emits inside it. A scheduled reopen gap in v2 lands at the
  first instant after the jump.
- **Rails.** The price level is clamped to `[tick, MID_CEILING = 1e9]`; the
  clock refuses rather than saturating at `u64::MAX`.
- **Tape identity.** `TAPE_PROTOCOL_VERSION` (30 today) advances on any
  change that could move a tape byte, unconditionally. The identity is stated
  once, in `crates/mogwai-data/tests/tape_version_prose.rs`'s phrasing, and
  a second statement in other words is a test failure. The crypto lineage's
  first six hours are pinned by FNV-1a hashes in
  `analysis/protocol9-tape-hashes.json` through `protocol9_tape_oracle`.
- **Provenance.** `CalibrationProvenance::{Uncalibrated, Fitted{corpus}}`
  already exists on the book knobs. v2's fitted/derived/declared ledger
  should extend that type rather than add a parallel one; `derived` is the
  missing variant.
- **Vocabulary.** `crates/mogwai-data/tests/glossary_vocabulary_prose.rs`
  checks prose against the glossary. Code comments use river, boat,
  passenger, water as defined; `client` is not used.
- **Build.** `brokkr check` and `brokkr check --gate`; four socket-backed test
  binaries run only under the gate.

---

## 7. Corpus and tooling state

### 7.1 What is on disk, and what the todo believes

The todo (Tape research v2 section, and the order-path calibration entry)
says the delivered MNQ corpus is TBBO only, that `mbp-1` is held server-side
at Databento and re-fetchable by job id, and that terabytes of DBN are
downloaded "on another host". That host is this one: `/speilelg/databento`
holds about a year of `mbp-1`, seven months of `tbbo`, 30 days of `mbo` and
`mbp-10`, and 1 s bars since 2010, for 63 CME products (see
`synthetic-tape.md` section 3 and `~/Claude/dbpull/HANDOFF.md`).

Consequence: the deferred calibration intake for `quoted_width`, `top_sizes`
and `trade_displacement_ticks` (todo, order path) has its data now, and
`depth_levels` and `depth_growth`, which the todo says cannot be honestly fit
from TBBO, can be fit from `mbp-10`.

The delivered MNQ corpus the lab reads lives at
`research/market-data/databento/mnqv/<month>.manifest.tbbo` inside the mogwai
tree, out of git: eleven months plus April 2026.

### 7.2 What the lab can read

`mogwai_lab::stream` is a Databento CSV-zstd contract for TBBO only. Required
columns: `ts_event`, instrument id, side, price, size, `bid_px`, `ask_px`,
`bid_sz`, `ask_sz`; prices as integers on the grid; book classified as
normal, locked, crossed or nonpositive. Nothing in either crate reads DBN.

Consequence for the programme's language decision: exploratory experiments
E1 to E4 run in Python against DBN with `databento-python`; only the final
intake estimators need a Rust reader, and that reader is new work.

### 7.3 Reusable pieces

- `mogwai_lab::session`: the CME session frame in exchange-local minutes.
  Session opens 17:00 the previous civil day, halt 15:15 to 15:30 (asserted
  to be exactly 15 minutes), close 16:00; segments named `overnight` and
  `post_halt`; `None` inside the halt or the daily break. The v2 phase
  taxonomy is finer than this and should be built beside it, not on it.
- `mogwai_lab::segments` and `mogwai_data::segment`: the cut and compose
  halves of the segment sampler. Four windows in exchange-local time: `asia`
  17:00 to 02:00, `london` 02:00 to 08:00, `ny-morning` 08:00 to 11:00
  (09:00 to 12:00 New York, half-hour lead-in), `ny-afternoon` 09:30 to 15:00
  (10:30 to 16:00 New York). The library stores nano-log-returns, inter-trade
  gaps, sizes, DBN aggressor letters, and a measured `open_gap_ret` per
  segment. Under the fitted direction this is the real-side comparison set:
  it already partitions real months by session phase, and the reopen gap is
  already measured per day.
- `mogwai_lab::characterize`: streaming ACF ring buffers, histogram
  quantiles, duration dispersion, zero-change fraction, per-second counts.
  The estimand layer, with the `AutoCorr` guard defect noted.
- `mogwai_lab::session_profile`: the calendar-conditional hourly profile fit
  with its preregistered thresholds. Superseded by per-phase fitting but the
  exposure and normalization reasoning carries over.
- `mogwai_lab::kernel`: bit-exact `splitmix64`, `tuple_mix`, Fisher-Yates,
  and four quantile conventions. Never compare a Python percentile with a
  Rust one without naming the convention; the todo records that trap for
  `analysis/asia_jump_probe.py`.
- `mogwai_lab::storage`: artifact, cache and scratch classes with a
  provenance token that folds in the git sha, crate version,
  `TAPE_PROTOCOL_VERSION` and fingerprint hash.
- `analysis/plot_tape.py` renders a bars CSV to HTML; `mogwai segments
  compose --type bars` and `gen --type bars` produce the CSV. The chart gate
  runs through this.

### 7.4 Closed-arc machinery, and the deferred ruling

About 25,000 lines across `mogwai-lab` and `mogwai-cli` are the compiled
machinery of the closed 12a and 12b protocols: `measure12a`, `aggregate`,
`stage_a_batch`, `arrival_screen`, `arrival_control`, `arrival_envelope`,
`select_windows`, `tick_composition_ratios`, `subcontract`, `sidecar`,
`sampler`, `cadence_feasible`, and in the CLI `count_curve`,
`ordered_counts`, `slow_geometry`, `stage_m`. Two artifacts of 86 MB and
57 MB are `include_str!`d, three sites outside `cfg(test)`, so three copies
ship in the binary. The owner deferred deletion until v2's shape was known.

Input to that ruling from v2's design: v2 needs `stream` (extended to DBN or
replaced), `session`, `segments` and `segment`, `characterize`, `kernel`,
`storage`, `fingerprint`'s provenance types, and the generator's `calendar`,
`quote`, `checkpoint` and seed derivation. It needs none of the modules
listed in the previous paragraph. The corpus-side machinery a successor
might want is the stream contract and the session math, both of which are
outside the closed-arc set.

---

## 8. Corrections to the programme's assumptions

- The programme assumed fitting tooling starts from scratch. It does not; see
  7.3. It also assumed the corpus could be read from Rust; it cannot yet.
- The programme placed the chart gate last. It goes first (section 1).
- The programme treated the per-phase volatility multiplier as a free fitted
  knob. Expect it near one (section 5); the honest framing is that it is
  fitted and its provenance records that it came out flat.
- The programme's kernel decay rates were described as "milliseconds to
  minutes". The measured clustering band says the slowest should reach an
  hour.
- The programme listed log-OU as an L1 candidate. As a volatility state that
  is fine; as an arrival-intensity mechanism it is retired by the v1
  ratios. The two uses should not be confused in E3.
- The programme proposed a `derived` provenance class. The tree's type has
  `Uncalibrated` and `Fitted`; `derived` and `declared` are additions to an
  existing enum, not a new scheme.
- The programme's phase table used Chicago time throughout. The tree's
  calendar is exchange-local week minutes with a UTC offset, and the v1
  winter failure says DST handling is a first-class intake step, not a
  detail.

---

## 9. Amendments owed to `synthetic-tape.md`

Listed rather than applied, by the owner's instruction to keep this a second
document.

1. Add experiment zero: a rendered week of the crude prototype beside real
   ES and MNQ, judged by eye, before E1.
2. Section 5 priors from section 5 here, with the v1 note as the source.
3. Section 4.4: the L0 volatility multiplier expected near one.
4. Section 4.3: kernel timescales spanning milliseconds to about an hour.
5. Section 4.2: log-OU is a volatility-state candidate only.
6. Section 4.5 and 6: the interface obligations of section 6 here, and the
   provenance enum extension.
7. Section 6: DST months as an explicit intake step; a DBN reader as a named
   piece of new work.
8. Section 7: the fill-path probe uses the venue's actual crossing rule,
   which as of protocol 27 walks the opposing quoted touch and a parametric
   ladder rather than slipping the last print.
9. Section 9: the reconciliations of section 3 here.
10. A short section stating the bar of section 2 here.
11. Added 2026-09-06, the largest amendment: sections 4.2 and 4.3 name a
    marked Hawkes engine as L2 and MSM as L1. On MNQ the two are one layer,
    a multi-scale log-Gaussian activity cascade from fifteen seconds to a
    month, and the price variance follows the count. The evidence, from
    260 real sessions: the envelope-normalised minute volume is lognormal
    to two decimals in every phase (a Hawkes count is not, and its
    near-critical tail overshoots the real p99 of 4.8 times the median);
    its autocorrelation is a mixture of a few exponentials at minutes and
    an hour, the same in every phase, which settles E1 for calendar-time
    kernels with per-phase amplitude; the range residual is explained by
    the square root of the volume residual plus Brownian sampling spread,
    which settles the time-change hypothesis of 4.4 in its favour; and the
    close is a martingale at every horizon, so `P+ -> P-` reversion and
    `T+ -> P+` impact are below what a minute bar sees. What the cascade
    does not model is Tier 1 below a second: sweep structure, queue
    depletion, sub-second inter-event law. Those are still E2's and E5's
    questions, on TBBO and mbo, and may yet want an excitation kernel at
    the event level under the cascade's rate. The correction in section 8
    that retired log-OU as an arrival mechanism is withdrawn at the minute
    scale: the v1 cells that failed carried a single timescale at the
    second level with a sigma several times the fitted one.
12. Added 2026-09-06, tape protocol 33: the Tier 1 layer below the second
    that amendment 11 left open is measured on the year's tbbo and landed.
    The real placement inside a second is a branching process, a fifth of
    all parent gaps under a millisecond at every hour and 100 ms bins
    dispersed 2.7 to 4.0 times uniform, which a Hawkes-type excitation
    under the cascade's rate gives and a Poisson count cannot; the real
    sign memory is a power law, not a Markov chain, and the order-splitting
    picture (Lillo, Mike and Farmer) lands it; and a CME multi-print sweep
    is a multi-level sweep nine times in ten. What the measurement found
    and did not land: the signed mid move after a parent is 0.48 ticks at
    one parent and 0.66 at a hundred, permanent, and the cascade's mid is
    independent of the side. `notes/synthetic-tape-micro.md` is the record.

---

## 10. Open questions for the owner

- Resolved 2026-09-06: v2 landed as a second walk inside `GeneratedSource`
  rather than a third lineage, selected per preset. The crypto preset stays
  on the old walk byte for byte and the protocol-9 oracle still holds; MNQ
  and MES run the cascade. Whether the old walk is retired once the crypto
  tape is refit on the cascade is the owner's call and not owed yet.
- Whether the segment library's real-side partition (7.3) is the reference
  set for the chart gate, which would make the four cut windows the
  canonical real-side views.
- Whether the 25,000 lines of closed-arc machinery can now be retired on the
  input in 7.4.
- Whether the `research/market-data/databento/mnqv` corpus in the tree is
  superseded by `/speilelg/databento` for every purpose, so the lab reads one
  root.
