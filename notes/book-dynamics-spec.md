# Book dynamics: the discrete book, spread state and depletion mechanism

The landing spec for the cascade's next protocol step, sparred to consensus
with codex on 2026-09-11 (session 01a09105-ed3f-7683-8f05-17b6ba73d92f, four
exchanges). This replaces the two cascade backlog entries it grew from in
`notes/todo.md`: the per-parent price change dispersion and the spread
dynamics item under the impact entry.

## Why

Two measured failures of the protocol 34 tape share one mechanism
(`notes/synthetic-tape-micro.md`, sections 6 and 7):

- The last print moves zero ticks between consecutive parents 21 percent of
  the time against 31 real, and three or more ticks 28 percent against 11.
  A Student-t innovation on a continuous mid rounded to the grid moves the
  touch on nearly every parent; the real touch moves only when a level is
  depleted. A heavier tail was tried and does nothing.
- The impact of a parent is 0.45 ticks in every phase where the real one is
  0.59 in Asia and 0.65 in London, because the tape's book is a constant
  two ticks while the real spread is one tick 21 percent of the time in
  Asia and 45 at the cash open.

The shared mechanism is a persistent discrete book with a stochastic spread
and effective depletion, replacing the derived-per-parent
`place_book(mid, quoted_width, top_sizes)` on the cascade path.

## The consensus mechanism

One paragraph, as agreed: a persistent discrete book with fitted stochastic
touch quantities and shared declared deeper depth; external latent moves
(diffusion, jumps, reopen gaps) applied through minimal projection before
execution; quantity-conserving parent walks followed by replenishment and
one joint projection against the updated permanent-information anchor;
emergent transient response fitted before adding any pressure channel;
published snapshots authoritative for venue crossing. Reporting splits
preserve execution quantities and prices, duplicate quotes are suppressed
with snapshot recovery for positioned readers, and fitting compares real
and generated books through the same parent-sampling convention.

### State

`CascadeState` (or a `BookState` it owns) carries, all `Clone`:

- bid ticks, ask ticks (the published touch);
- the spread state and its dwell counter;
- touch quantities per side, and residual quantities on exposed deeper
  levels;
- the latent-to-book residual (or equivalently the current admissible
  band), and any staged post-trade transition;
- a dedicated book rng stream and a dedicated reporting rng stream, both
  derived from the seed, separate from the cascade's main stream.

The latent efficient price stays continuous and keeps the cascade's
diffusion, jumps and gaps. The propagator's permanent component moves the
latent price; the transient register is removed. Mechanical depletion moves
the book; nothing moves both for the same economic event.

### The projection rule (reconciliation)

Minimal projection, never a snap to the latent mid. In ticks, for latent
target x, spread s and slack a = 1, the admissible bid placements are

    A(x, s) = { b integer, b >= 1, abs(b + s/2 - x) <= s/2 + a }

and projection of a candidate book with midpoint m* selects the admissible
b nearest m*, ties broken by a fair draw from the book rng. An admissible
existing placement is preserved exactly; an inadmissible one moves only far
enough to restore the bound. Near the one-tick floor the target is clamped
so the set stays nonempty; symmetry cannot hold there and is not claimed.

Applied twice per parent:

1. Before the pre-trade quote: advance the latent target by the parent's
   external diffusion, jump and reopen gap; project the carried book.
   A reopen trade meets the reopened book, not the stale touch.
2. After the children: form one candidate book from consumption and
   replenishment, advance the latent target by the permanent information
   component, and project the candidate once against the updated target.

Depletion and permanent information enter different arguments of one
projection, so movement the depletion already made toward the target is
credited rather than duplicated. A projection that pulls an over-swept book
back is replenishment in the model. The mechanical displacement and the
projection displacement are recorded separately in diagnostics, including
how often they oppose, because their cancellation shapes both the response
curve and the trade-price pmf.

Known and accepted: the band leaves conditional predictability given the
latent-to-book residual. It is bounded (the published price error never
exceeds the band), so long-horizon variance growth is preserved; the
finite-horizon effects at the minute (close sd, range, autocorrelation,
variance ratios) are acceptance criteria of the fit, not assumptions.

### The spread process

A persistent Markov state over spread values (one and two ticks carry 86
percent of the real mass pooled; the tail is part of the fit), with
transition probabilities conditioned on phase, an observable intensity
proxy and an observable volatility proxy. Persistence is required: an
independent draw per parent can match the marginal pmf while producing
quote flicker and wrong depletion dynamics. Phase enters explicitly even
beside activity, because Asia and the open have different liquidity
behaviour at overlapping instantaneous rates.

Parity: changing between odd and even spreads moves the midpoint lattice
between integer and half-integer. The side moved preserves the midpoint
where parity permits and randomizes symmetrically (book rng) where it does
not. Always widening the ask or narrowing the bid is drift and is
forbidden.

### Execution: the quantity-conserving walk

Per parent: draw the parent quantity, then execute against the struck
side's occupied levels in order, consuming min(remaining, level quantity)
at each, advancing only when a level exhausts. Levels crossed and levels
exhausted fall out of the walk instead of a solved step probability;
`levels_mean` becomes a calibration target. One-tick stepping assumes every
intervening level is occupied, which is the declared ladder's shape.

Depletion is effective, not literal queue truth: TBBO cannot see
cancellations or order-level queues, and the fit names it an effective
depletion model.

Touch quantities are a fitted stochastic state. Replenishment after
depletion draws from a fitted replenishment law, which is not the observed
pre-trade touch size distribution: large queues survive longer and partial
consumption leaves residuals, so the replenishment law is fitted such that
the simulated pre-trade distribution matches the observed one. Exposed
deeper levels keep their exact residual quantities; a replacement or
replenishment of them is a separate modeled action after consumption.

Deeper quantities beyond the touch take the declared preset ladder
(`depth_levels`, `depth_growth`, both stamped uncalibrated pending mbp-1),
one definition shared verbatim by the generator's walk and the venue's
crossing. A parent quantity exceeding total declared depth meets an
explicit exhaustion rule shared by both paths; the generator never quietly
appends levels the crossing lacks.

### Reporting: the same-level split

Real multi-print parents are single-level 9.3 percent of the time (several
match events at one price inside one `(ts, side)` group). The reporting
layer may split one level consumption into two records with a fitted
probability: only a consumption of at least two minimum-size units splits,
the integer split u satisfies 1 <= u <= q - 1, both records carry the same
price and side, and total quantity, final queues and first and last
execution prices are unchanged. The reporting rng is its own stream so
reporting parameters leave book mechanics untouched.

Feasibility identity, to check at fit time: with p the probability of a
multi-level execution and q the unconditional single-level multi-record
mass, the conditional share is p / (p + q), so 0.907 requires
q = p * (1 / 0.907 - 1), and the same q must be consistent with the
child-count pmf and mean, not just the conditional share.

### The wire contract

- Strict monotone stamping stays: every record steps the clock by one
  intra-event step (one microsecond), with checked advancement; timestamp
  overflow is exhaustion, never a saturated tie.
- Per parent, in order: stage-1 projection; the pre-trade quote, published
  only if any of the four values (bid price, ask price, bid size, ask
  size) changed since the last published state, and always published at
  the origin; the child and split records; the post-trade book made
  effective at last-record-plus-one-step, published under the same
  four-value rule. The post-transaction instant is reserved even when its
  quote is suppressed, so deduplication does not change the schedule. The
  next parent is floored strictly after it.
- The parent is an atomic book transaction: fills between the pre-trade
  quote and the post-trade instant cross the pre-trade book; the child
  prints are reports of that transaction.
- A positioned source exposes the last published book snapshot as of its
  consumed prefix (for `scan_triggers` and any reader that learns the book
  from quotes), supplied as reader initialization, never synthesized as a
  tape event.
- The whole parent is staged as an emission plan with an exact wire count,
  final timestamp and replay cursor; `next_tick` and `advance_parent`
  execute the same plan, and a checkpoint may be taken at any cursor
  position. `CheckpointIndex::extend_toward`'s per-parent event count and
  end instant follow the plan, not `1 + child_count`.

### The preservation contract

Statistical, stated plainly: separate rng streams remove direct draw
consumption coupling between book/reporting mechanics and the cascade's
exogenous draws, but the wire clock feeds back into the arrival process
(`draw_parent` spawns children from the floored timestamp;
`fill_next_bucket` shortens the remaining second by it), so emitted
timestamps, arrival counts and ordering are not promised identical under
book or reporting parameter changes. Acceptance is by the gates, per the
correctness contract. The diagnostics record raw-to-emitted parent delay
quantiles and the maximum, and the sub-millisecond gap share is checked
with staged parent identities, because a one-microsecond displacement can
cross a classification boundary.

### Generated-side measurement

The generated diagnostic output carries an explicit parent identifier from
the staged plan. Generated statistics are measured on it; the legacy
ten-microsecond grouping is reported beside as a discrepancy check, since
quote records do not bound that heuristic and post/pre quote adjacency can
merge same-side parents. Generated measurement consumes actual published
quotes: the mid inferred from first-print-minus-half-width in
`micro_stats.py` is invalid under a stochastic spread and is retired for
candidates that publish books.

## Measurement additions (real side, `analysis/tape-v2`)

The extraction (`micro.py`) already retains per-print records with the
pre-trade book, sizes, counts and sequence; no re-extraction is needed.
`micro_stats.py` grows, per phase and pooled, conditioned on elapsed time
where stated:

- the spread transition matrix between parent observations and the dwell
  (in parents and in wall time), with elapsed-time conditioning and
  explicit censoring at closures and phase boundaries;
- the midpoint change pmf between consecutive parents, separate from the
  last-print pmf (the latter contains bid-ask bounce);
- the depletion witness: the next pre-trade touch moved through the struck
  side, conditioned on parent size versus touch size (less, equal,
  greater), levels crossed, spread and elapsed time;
- the touch size distribution conditioned on spread state, and both-side
  sizes with imbalance and next-observation changes;
- parent size versus touch trichotomy split by spread, phase, side and
  levels crossed;
- the signed mid response at 1, 10 and 100 parents conditioned on the
  initial spread;
- the single-level multi-print mass eligible for a quantity split
  (quantity at least two at one level).

Identification limits, disclosed beside the fitted parameters: consecutive
pre-trade books do not isolate the preceding parent's effect (intervening
cancellations, limit orders, replenishment and unobserved spread
transitions sit between observations); a sampled transition matrix is
between parent observations, not underlying quote changes; an observed
spread run is not proof of dwell; touch movement is a response witness,
not an identified depletion; an unchanged touch does not prove absence of
depletion. The data selects an effective joint model and cannot uniquely
separate hidden depletion from replenishment.

Intensity and volatility conditioning uses the same observable proxies on
both sides (local parent rate over a trailing window; a short-horizon
realized-move proxy that excludes the response being predicted, to avoid
circular conditioning). Phase alone does not substitute.

## Fit protocol

Causal order, fitted jointly rather than as independent marginals, phase-
balanced so the open cannot buy the pooled pmf while Asia stays wrong:

1. Spread transition model with dwell, validated on marginal and
   transition statistics, out of sample by month.
2. Effective depletion transitions from the witness, conditioned on the
   size-versus-touch trichotomy and levels crossed.
3. The projection rule's slack against midpoint transition statistics,
   never against trade-price transitions.
4. Impact jointly with depletion: signed pre-trade midpoint response at 1,
   10 and 100 parents per phase, with the permanent component estimated
   after mechanical transitions are present. The protocol 34 constants
   (0.30 permanent, 0.15 transient, 0.98 decay) are not carried over.
5. Transient response: emergent-first. The sign memory integrates the
   permanent kicks (R(l) = g * sum of C(j) up to l), so the response curve
   is constrained, not arbitrary. The failure criterion for adding a
   channel is joint: lags 1, 10, 100, their phase dependence and the
   minute variance ratios. Too little growth at 10 with room at 100 admits
   a decaying signed pressure in depletion probabilities; too much growth
   through 100 wants weaker response to predictable flow or stronger
   recovery, never more same-direction depletion. Never a second mid
   mover.
6. Validate the composed observable through the parent-sampling
   convention: trade-price change pmf, midpoint change pmf, spread
   marginals and transitions, response curve, minute close sd, range,
   return autocorrelation and variance ratios, per phase, monthly
   holdouts.

The prototype (`proto_book.py`, to be written beside `proto_micro.py` and
`proto_impact.py`) simulates the joint model on the splitting sign model
and is measured through the same observation process as the real side.

## Landing rules

- This moves generated quote and trade bytes: the landing owes the next
  unspent tape identity, golden re-blessing in the same change, the fit
  tolerances re-run, and a rendered chart under the owner's eye (the
  standing tape-generation gate).
- The venue crossing consumes the published book: published bid, ask and
  sizes are authoritative, the deeper ladder is the one shared definition,
  and the generator's consumed quantities are the same quantities crossing
  sees. Reading mutable `CascadeState` from the venue is forbidden; the
  snapshots come through the tape.
- The non-cascade legacy walk keeps `place_book` and `quoted_width`
  byte-for-byte.
- `trade_bounce_ticks` retires on the cascade path: prints land at the
  published touch on the aggressor side.
- Sequencing: measurement additions first, then the prototype fit on the
  real targets, then the Rust mechanism with transcribed constants. The
  Rust landing does not ship ahead of fitted constants.

## Prototype findings, 2026-09-11

The measurement additions landed in `micro_stats.py` and ran on the real
year; `proto_book.py` implements the joint model and reached a pooled
score of 0.157 (mean absolute log miss over twenty statistics), with the
response curve at 0.45/0.65/0.62 against 0.48/0.65/0.66 real. The same
configuration fits the open at 0.158 untouched; Asia reaches 0.220 with
only phase knobs moved (thinner replenishment, lower one-tick target
share, higher permanent impact), which is the phase dependence the
mechanism exists to carry. What the fitting established, binding on the
Rust transcription:

- The real per-phase measurements broke the naive spread-impact story
  before any simulation: impact at spread 1 exceeds impact at spread 2
  (0.51 against 0.41 pooled, 0.87 against 0.74 in Asia), mediated by the
  thinner touch at narrow spreads (median 2 at spread 1 against 4 at
  spread 2). The spread's effect on impact runs through depletion
  probability, not through half-spread arithmetic.
- The emergent-first transient route measurably failed in the sanctioned
  direction: with the permanent component alone sized to lag 100, lags 1
  and 10 sit at half real. The adopted channel is the residual impact
  term on the latent anchor (the propagator form: permanent about 0.31,
  transient about 0.32 decaying at 0.9 per parent, pooled), which the
  projection credits mechanical movement against, so nothing moves the
  book twice.
- A witnessed depletion needs the un-struck side to follow in with a
  fitted probability (about 0.6); without it the immediate response
  cannot reach half a tick. And a witnessed depletion that no quote
  followed must skip the same-parent narrowing step, or the relaxation
  cancels the very move the witness measures.
- The spread relaxes toward a drawn target state, not toward one tick;
  Asia wants a third target state (29 percent of its books are 3 ticks
  or wider), which the two-state draw cannot carry.
- The diffusion needs the heavy tail and the persistent lognormal sigma
  level (log-sd near 1.5 at the parent scale) to reach the fast-bucket
  midpoint tail; in the engine this is the cascade's own second_sigma,
  supplied for free.
- The fast-bucket pmfs (`mid_change_pmf_fast`, `abs_change_pmf_fast`)
  are the honest targets for a parent-indexed model; the pooled pmfs mix
  every elapsed gap.

The per-phase pass completed the picture: with the third spread target
state and widening generalised to any spread below the drawn target (a
two-tick book in a thin phase loses its touch to cancellation as readily
as a locked one), every phase fits the one mechanism on phase-varying
knobs - pooled 0.157, ny_open 0.158, ny_close 0.172, asia 0.193, london
0.220. The asia/london configuration against the open/close one moves
every knob monotonically with phase activity: replenishment mean 1.05 to
1.6, one-tick target share 0.25 to 0.65, widening 0.45 to 0.24,
permanent impact 0.44 to 0.31, transient 0.48 to 0.32, depletion at
size-ge-touch 0.7 to 0.6. That monotonicity suggests the preset carries
the book knobs as a link on the envelope's own activity curve v(m)
(base plus slope on log v, two numbers per knob fitted from the phase
points) rather than four hand tables - the conditioning is the
deterministic time-of-session, which is what the spar's phase
requirement asked for, not the instantaneous rate it warned against.

The monthly holdout of the pooled configuration scores 0.124 to 0.334
against each of the fourteen months individually (median near 0.175,
2026-06 the outlier), inside the month-to-month variation of the real
statistics themselves, so the fit generalizes rather than memorizing
the median month.

All pre-transcription gates that can run without the engine are now
closed: the mechanism consensus (two spar sessions), the real-side
measurement, the per-phase prototype fits, the deterministic-depth
re-check, and the monthly holdout. What remains rides on the Rust
implementation itself: the minute gates on a composed walk, the golden
re-blessing, the tape protocol bump, and the chart under the owner.

## Rust transcription progress, 2026-09-11

Stage 1, the pure mechanism: `crates/mogwai-data/src/generated/book.rs`
- `DiscreteBook` with the six-step parent transition, the venue-identical
`ladder_next_units`, domain-separated book and reporting rng streams, and
`BookDynamicsConfig`/`BookPhaseKnobs` (per-phase tables keyed by minute of
session). Unit-tested and green.

Stage 2, the wiring into `source.rs`:
- `GeneratorScalars.book: Option<BookDynamicsConfig>`, validated to require
the cascade and (still owed) an integral size grid; the venue deserializes
`[instrument.generator.book]` with no new override key, exactly as cascade.
- `draw_cascade_parent` factored out of `begin_cascade_event`; the placed
book path adds the cascade propagator, the discrete-book path
(`begin_cascade_book_event`) takes only the external move and drives the
`DiscreteBook`, applying the book's own anchor impact to the latent mid.
- The atomic-parent wire contract as a staged `BookPlan`: a reserved-slot
schedule (pre-quote slot 0, trades 1..=n, post-quote slot n+1, one
intra-event step each), four-value quote suppression via `last_published`
with instants still reserved, and the clock floored past the last slot.
- `next_book_tick` pops the plan; `advance_for_checkpoint` returns a
`WireAdvance` (end instant, emitted record count) that both `seek_to` and
`CheckpointIndex::extend_toward` consume, off the old
`1 + child_count`/fixed-stride arithmetic.

Stage 2 is tested and green (gated check, socket suites included). The
book path's determinism is pinned by four integration tests in
`generated/tests.rs`: the integral-grid refusal, a monotone wire stream
carrying both frames, and byte-identity across both a `seek_to` resume
and a `CheckpointIndex` resume - the load-bearing checks that the
emission plan, quote deduplication and both rng streams replay
deterministically. The integral-grid validation landed in `try_build`.

The venue crossing already consumes the published book, verified rather
than owed: `fills.rs`'s `MarketReading` takes `bid_px/ask_px/bid_sz/
ask_sz` from the tape-published quote (`market_snapshot_reading`) and
resolves `depth_levels`/`depth_growth` from the preset - the same two
constants the generator's frozen ladder walks, with `cross_book`'s
`floor(size * growth).max(increment)` recurrence identical to
`ladder_next_units` on the integral grid. A book preset publishes its
touch through the ordinary quote path, so the crossing sees it with no
new wiring, and the two ladders agree by construction.

Gated on a served book preset, not owed now:
- The positioned-reader snapshot recovery. Real, but only reachable once
  a book preset is served with dedup active - and whether dedup actually
  starves `market_snapshot_reading` depends on its `VOL_WINDOW_NS`
  lookback (which already reaches back before the triggering print)
  versus the realized dedup gap, a served-preset-time measurement rather
  than an assumption. Build it when a preset makes it measurable.

The preset landed: the MNQ `[instrument.generator.book]` table (three
phase rows), `TAPE_PROTOCOL_VERSION` bumped to 35, the Stage A manifest
re-blessed, the gated prose moved to "next takes 36", the venue config
key list and provenance extended - workspace green (1629 tests).

## The transcription does not yet reproduce the fit, 2026-09-11

Measuring the engine's book tape against the real year (a generated week,
`micro-stats --label p35`) caught that the headline fix did not transfer.
The per-parent last-print change - one of the two failures the book
exists to fix - is essentially unmoved from protocol 34 and if anything
slightly worse: dprice 0/1/2/3+ reads 0.21/0.29/0.22/0.28 against the
real 0.31/0.39/0.19/0.11, where protocol 34 was 0.24/0.32/0.24/0.21. Every
other measured statistic (rate, gaps, sweep, sign memory) stays on the
real band; only the price-change did not improve. The prototype scored
this same statistic near real, so the gap is in the transcription, not
the mechanism.

The diagnosis, not yet confirmed by measurement: the prototype fitted its
diffusion as a free `sigma_e` (about 0.35 tick per parent) and the
stickiness knobs - the projection slack, the spread targets - were fitted
against that. The engine drives the book against the cascade's actual
per-parent diffusion, which at MNQ's `event_log_sigma` is roughly twice
that (~0.75 tick per parent, the value that makes the minute close sd
match). A book whose slack and spread law were fitted at half the
diffusion cannot stay sticky at the true one: the latent mid leaves the
band more often, the touch chases it, and the print (now at the touch,
bid-ask bounce of the full spread) swings more than the old placed book
did off the rounded mid. The prototype divorced its diffusion from the
cascade; the engine exposed it.

What this needs, and it is a real fork rather than a hack:
- A quotes dump from the engine (a new `gen --type quotes` mode) so
  `micro-stats` can measure the engine book's spread pmf, midpoint change
  and depletion witness end to end - currently impossible, because a
  trades CSV carries no book and only the last-print change is visible.
  Every book target except dprice is unmeasurable on the shipped tape
  without it.
- Either a re-fit of the book knobs with the diffusion pinned to the
  cascade's real per-parent sigma, or a sparred design change to how the
  latent diffusion enters the book (the book absorbs sub-level moves, so
  the mid the touch projects against may want damping rather than the raw
  cascade mid). This is the open question.

The preset, the tape bump and the wiring stand as scaffolding for that
work; the tape is not yet fit to bless. Nothing is committed.

## The real bug and what the refit must target, 2026-09-11

The diffusion-scale story above was a red herring, and codex caught the
error in the reasoning before any refit ran: higher diffusion cannot make
the spread fail to narrow, because projection preserves the spread and the
narrowing depends only on the book parameters and the witness flag, not on
the anchor's displacement. The diffusion mismatch and any spread mismatch
are two independent channels.

The quotes measurement (a new `gen --type quotes` mode) then found the
actual bug, and it was neither: the spread was frozen at exactly 2 ticks
across every one of 4.8M quotes. The book was not running at all. The MNQ
`[instrument.generator.book]` table parsed and passed the generator key
allowlist, but `PartialGeneratorScalars` - the overlay type the venue
deserializes a preset's generator table into - had no `book` field and
silently dropped it, so `GeneratorScalars.book` arrived `None`, `self.book`
was `None`, and `begin_event` dispatched to the placed constant-width book
(`quoted_width` 2). The tape had been the protocol-34 placed book wearing a
protocol-35 version number. Fixed by carrying `book` through the overlay
and its `From`; pinned by
`the_mnq_preset_carries_its_book_and_cascade_into_the_scalars`, which is
the guard the whole class needed - the exhaustive-destructure guard only
catches fields added to the partial type, not to `GeneratorScalars`.

With the book actually running, the mechanism works and dprice moved toward
real: pooled 0.23/0.40/0.185/0.184 against real 0.31/0.39/0.19/0.11 (was
0.24/0.32/0.24/0.21 under the placed book), the 1-tick and 2-tick buckets
now matching. Two coupled defects remain, one root:

- The spread tail is too heavy: 1t 0.36, 2t 0.38, 3t 0.16 with a tail past
  10 ticks, against real 0.43/0.43/0.14 and nothing wide. `relax`'s widen
  is capped at `MAX_SPREAD_TICKS`; the depletion `recede` in `settle` is
  not, so a multi-level exhaustion widens the ask unboundedly and the
  follow-in only sometimes restores it.
- Parent size mean is 16 against real 2.24, inflating children (1.26 vs
  1.16) and levels (1.24 vs 1.10). This is downstream of the spread tail:
  `replenish` thickens the touch by `touch_by_spread^(spread - 1)`, so a
  wide-spread book carries an enormous touch, and the size-match channel
  (42 percent of parents) then takes that touch as the parent size. Fix
  the widening and this collapses with it.

The refit (option A, codex's ruling) targets the depletion widening cap
and the `touch_by_spread` exponent, then re-runs the quotes and trades
measurement together. The minute-and-above stays preserved throughout (it
was preserved even under the placed book). This is the open work; the
mechanism, the wiring and the measurement surface are all in place for it.

## Current tree state, 2026-09-11 (book unshipped pending the refit)

The book is not fit to bless, so it is not shipped: the MNQ preset table,
the `TAPE_PROTOCOL_VERSION` bump to 35, the Stage A manifest re-bless and
the gated prose were all reverted, and MNQ is back on its blessed
protocol-34 cascade tape. Shipping the book on MNQ would have replaced a
blessed tape with an unfit one, which the standing chart gate forbids, and
it also broke the closed-12b measurement machinery that runs on the MNQ
preset (the arrival screen inherits `book` without a cascade; the control
walk finds no vol-traced parent because the book path emits no trace).

What stays landed and green (1630 tests) as tested-but-unshipped
capability: the `DiscreteBook` mechanism (`book.rs`), its wiring into the
source behind the emission plan, the `gen --type quotes` measurement mode,
the `PartialGeneratorScalars` overlay fix with its regression test, and the
book validation (requires cascade and an integral grid). `GENERATOR_KEYS`
carries `book`, so a book preset is accepted when one is authored.

Owed before the book can ship, in order:
1. The option-A refit in the prototype (cap the depletion widening, refit
   `touch_by_spread` and the coupled knobs) against the real cascade
   innovation path, verified by the quotes/trades micro-stats.
2. The closed-12b integration: the book path must emit a `VolTrace` per
   parent, and the arrival-kernel swap must strip the book (it requires a
   cascade), so the measurement machinery survives a book-carrying preset.
3. Re-author the MNQ preset table, re-bump the tape, re-bless the manifest
   and prose, and render the owner's chart - noting the book's effect is
   sub-second and the chart's role is only the macro non-regression check.

## Second spar, 2026-09-11: parameterization and transcription rulings

A fresh codex session (01a09153) audited the phase-link proposal and the
transcription plan against the engine source. Consensus, in its words:
preset-declared phase tables with state-preserving law changes, a frozen
shared finite ladder per parent with explicit post-transaction renewal
and exhaustion replenishment, and a positioned reader carrying the
published snapshot of its consumed prefix, subject to the scheduled
deterministic-depth prototype re-check. The substance:

### Phase tables, not an activity link

The activity link was rejected on identifiability: two jointly-fitted
configurations are two effective points and cannot identify per-knob
slopes; the confounded knobs (thin replenishment raises depletion,
depletion and follow-in raise short-lag response, the anchor terms
compensate through projection) make phase-optimal values non-unique
optima, not observations; and half the knobs need bounded links
(probabilities logit, the spread target shares a simplex), so plain
log-linear was wrong regardless. The phase coordinate stays until a
matched-activity residual test falsifies it: fit a linked model against
minute-conditioned targets and show phase and session-limb labels have
no remaining predictive value in matched v(m) bins. That test is the
standing gate for ever collapsing the tables.

The preset carries the book knobs as per-phase tables with the phase
boundaries declared in the preset as minutes of session, data beside
the envelope rather than a new calendar frame, each knob with its own
provenance entry. A boundary is a law change only: the persistent book
state crosses untouched, no reset, the first parent after it simply
draws under the new law. Continuity is not load-bearing in either
direction; what binds is no artificial state reset, correct censoring
across closures, and acceptance statistics around the boundary.

### The frozen ladder: deep book deterministic, touch stochastic

The deep-residual versus declared-ladder mismatch is resolved by making
the declared ladder the deeper book on both sides, with stochastic
persistence confined to the two published touch quantities. The rule,
exactly:

- Construct each side's finite ladder from the pre-trade snapshot with
  the shared multiply-and-floor arithmetic (the same `depth_levels` and
  `depth_growth` the crossing resolves).
- Walk that frozen ladder once per parent, original level indices,
  consumption subtracted exactly once. Never rebuild mid-parent: the
  stacking hazard is a size-5 buy against quantities 2, 4 consuming 2
  and 3 - rebuilding from the exposed residual of 1 mid-walk would show
  later consumption 1, 2, 4 where the venue sees the original levels.
- The post-trade touch is the surviving level's exact residual;
  exhausted levels are skipped and a zero residual is never a published
  occupied touch.
- After effective depletion, replenishment, follow-in, relaxation and
  projection, the next deeper ladder derives from the final published
  touch. This is an explicit renewal of deeper liquidity between
  transactions: the old deeper quantities are discarded (publishing
  touch 1 means the next book is 1, 2, 4 even where 8 used to sit
  behind), because retaining them would need published state the wire
  does not carry.
- Full exhaustion shares the crossing's partial-fill arithmetic and
  never appends an execution level to finish the requested parent;
  requested, executed and unexecuted quantities are recorded
  separately, reporting records sum to executed, and the post-trade
  replenishment rule creates the next valid touch.

The named statistical cost: deep liquidity variation conditional on
touch size is lost, narrowing the conditional sweep-length
distribution. The prototype re-check must show touch replenishment and
ladder growth jointly retain the walk-through share (0.08), the
sweep-length distribution and the child-count pmf with feasible
reporting splits - matching the 0.907 conditional share alone leaves
the question open, since splits supply the single-level mass
(0.08 * (1/0.907 - 1) = 0.0082 unconditional) but cannot repair the
walk-through share.

### The positioned-source interface

Snapshot recovery ships as a positioned-source interface, not a
`TickSource` method: positioning returns the reader together with its
initial published snapshot, timestamp included, under one invariant -
the snapshot describes the prefix strictly before the reader's next
event. Every book reader (`scan_triggers`, `book_reading`,
`market_snapshot_reading`) initializes from it and applies returned
quotes normally. `seek_to` consumes the event it returns, so a
positioning wrapper buffering that event must capture the snapshot
before the buffered event; a staged parent's post-trade snapshot is not
visible before the replay cursor reaches its publication.

### The deterministic-depth re-check, and the size-match channel

The frozen-ladder re-check passed, and improved both phases once the
walk-through gates joined the score: pooled 0.117 and Asia 0.165 over
twenty-two targets (the twenty plus size-ge-touch and size-gt-touch).
Two findings landed during it, both binding on the transcription:

- The frozen ladder fits better than the stochastic deep residuals it
  replaced (pooled 0.147 against 0.157 before the gates joined; Asia
  0.161 against 0.193). The named cost - lost deep-liquidity variation -
  did not bite at these targets.
- Parent size is endogenous to the touch: the real eq arm of the
  trichotomy is 0.46 pooled (size-ge-touch 0.542 less size-gt-touch
  0.079), because a taker sizes the order to the displayed quantity. A
  size law independent of the touch cannot express that at any
  parameter value. The model draws the parent size as exactly the
  displayed touch with probability p_size_match (0.42 fitted), else
  from the independent size law. This channel alone moved the pooled
  score from 0.189 to 0.117, fixing the trichotomy, the spread pmf and
  the dprice tail together. In the engine it replaces the unconditional
  parent-size draw on the cascade path; the declared size law remains
  the non-matching arm.

### Transcription corrections from the source audit

- The rng streams are domain-separated derivations from the realization
  seed with stable labels, never drawn from the main stream (which
  would couple the cascade realization to their creation); the
  constructor does not retain the seed today, so it is passed at
  construction. The streams live in `CascadeState` so cloning and
  checkpointing carry them automatically.
- The emission plan is immutable resolved data (exact wire schedule,
  suppressed-quote reservations, split records, final reserved
  timestamp); the replay cursor is state of the consuming source, not
  of the plan. `ParentSummary` as it stands (timestamp, child count,
  fixed stride) cannot describe any of it and is replaced by the plan
  summary.
- `GeneratedSource::seek_to` carries the same obsolete
  1 + child_count arithmetic as `CheckpointIndex::extend_toward`; both
  consume the plan summary.
- The legacy branch condition is cascade-book configuration presence,
  not calendar presence - calendar-backed non-cascade paths exist.
  `trade_bounce_ticks` stays constructed and consumed exactly as today
  on every non-cascade branch; byte goldens pin the calendar-less and
  the calendar-backed non-cascade streams.
- Checked timestamp advancement everywhere on the new path: the
  current `saturating_add` sites would produce tied timestamps at
  overflow where the contract says exhaustion.
