# Bug sweep - open items

What remains from the six read-only hunt agents over the workspace and the fix
batches that followed. This is a live tracking doc, not a record: an item that
gets fixed, refuted, or settled as a deliberate decision is REMOVED from here
entirely. The record of what was fixed lives in git history; the reasoning
behind a deliberate decision lives in the code comment or `reference/` doc where
it will outlive this file. So everything below is a CURRENT gap.

Every item here is open because it needs a design decision or a non-trivial
build, not because nobody got to it. The mechanical work is done.

IDs are the original sweep's: D = data path, AD = adapter data half,
AE = adapter exec half, X = cross-boundary seams, F = found during fix batches.
Gaps in the numbering are resolved-and-removed items.

---

## D16. The seeded tape realizes multi-hour trade deserts, so the live region can be near-silent for most of a day

`crates/mogwai-data/src/generated/source.rs` (`next_duration_ns`) with the
committed `ACD_PERSISTENCE = 0.9935`, `ACD_FEEDBACK_SHARE = 0.08`, and Weibull
shape 0.6 innovations. Severity medium-high for live-feed fidelity; confidence
high on mechanism (measured, anchor-independent).

Measured on the default BTCUSDT profile (fixed FNV symbol seed, so this shape is
baked into the tape): hours 0-5 of tape run dense (460-1000 ticks/hour, the
expected ~7s cadence), then the walk enters a slow ACD excursion and hours 6-23
realize 2-70 ticks/hour with inter-trade gaps up to ~48 minutes. Over 96h the
tape alternates bursts and 6-18-hour deserts; the mean per-tick duration stays
~7-8s (so the per-tick realism gate is blind to it), but wall-clock-wise the
venue goes near-silent for most of a day at a stretch - nothing a real BTCUSD
tape does.

Mechanism: psi decays per TICK (persistence 0.9935, ~106-tick half-life), so a
high-psi state self-prolongs in wall time - at 30 min/tick a hundred ticks of
decay is days of tape. Heavy-tailed shape-0.6 innovations kick psi up by ~alpha
times a single huge draw, entering the state; the per-tick fit never constrained
the wall-clock dwell. Consequences: a live subscribe anchored sim-now (always
~backfill_horizon past origin) can sit hours between ticks, making AD12's missing
dead-feed watchdog bite harder; `current_price` for a market order in a desert
stamps the NEXT tick's price, up to ~48 min ahead of the clock; density-sensitive
tests are inherently flaky (`trades_window_is_clamped_at_sim_now` was widened to a
6h window as mitigation).

Why this is NOT a clean, separable bug (the load-bearing finding): the three ACD
constants are not fitted per-symbol - they are hand-tuned in
`generated/consts.rs` specifically to land TWO committed fingerprint duration
bands, `duration_dispersion_index` in [131.7 .. 4608.9] and the
`duration_acf_anchor` vector. The `consts.rs` comment states the tension in so
many words: the dispersion statistic `var(d)/mean(d)` "is dominated by the single
largest gap" under a shape-0.6 (heavy) innovation tail, and the pair was
deliberately backed OFF the anchor extreme (4608.9) toward the band's lower
interior (~190) to keep unlucky seeds from exploding past the band. The
`realism()` gate (`generated/tests.rs`) then measures BOTH bands on the REALIZED
tick-to-tick gaps (`tick.ts_event` differences, not the internal event-time
`duration_s`) over a DRAW of 2,000,000 ticks - at ~7s mean cadence that is ~160
days of tape, so the deserts sit squarely inside the gated sample and are what
push the measured dispersion (~190 on seed 42) and the duration-ACF up into band.
So the deserts are not an artifact bolted onto the fit - they ARE the realized
mechanism that achieves the committed duration realism. Remove or bound them and
the dispersion very likely falls through the 131.7 floor and the duration-ACF
signature collapses; `realism()` goes red.

Sharper decomposition of the two bands: the DISPERSION band only needs big gaps
to EXIST (isolated heavy-tailed gaps are realistic - real tapes have them). The
duration-ACF band is what demands they CLUMP TOGETHER (autocorrelated gaps: quiet
begets quiet), and clumped big gaps ARE the deserts. So it is specifically the
committed duration-ACF target - the near-unit-root persistence 0.9935 - that
mandates deserts; isolated big gaps alone would satisfy dispersion without them.
If the ACF target is faithful to Kraken (plausibly - real durations
autocorrelate), the deserts are faithful in KIND and wrong only in wall-clock
SCALE (18h, which real BTCUSD never does), because NEITHER committed target
constrains wall-time dwell. That wall-clock-blindness of the duration targets is
the actual root gap.

Purpose framing (what the duration model is FOR): mogwai exists to exercise
broadarrow's LIVE path with a realistic market-data backdrop plus adversarial
execution divergences. The duration model serves the BACKDROP - realistic arrival
cadence (bursts and lulls, not a metronome) so the live path runs against a
believable feed. The fingerprint duration bands are a CREDIBILITY PROXY for that
backdrop, not the product; broadarrow does not care whether dispersion is 190 or
150, it cares that the feed paces believably and the divergences fire. A silent
feed is a legitimate TEST CONDITION in the abstract (it should stress the AD12
dead-feed watchdog, the bar-stall, and market-order `current_price` staleness).
BUT an 18h desert on the DEFAULT tape means that for most of a day mogwai serves
almost nothing, which defeats its own purpose of exercising the live path: you
subscribe to run a strategy against divergences and the venue is asleep. So the
dispersion band is INSTRUMENTAL, and when hitting it produces a backdrop that is
unusable for the actual job, loosening the band is legitimate - the proxy
overshot, the product is unharmed.

`mogwai gen` exists to make this visible: `gen --type bars --length 1d
--interval 5m` renders the deserts as flat zero-volume runs in the `trade_count`
column.

Options, with honest costs:
- A. Accept and document. Zero code risk. Frame the deserts as the cost of the
  committed duration realism and document the consequences (a live subscribe can
  idle for hours; market-order price staleness up to ~48min; AD12 must tolerate
  it; adapter-fabricated bars stall; density tests need wide windows). The
  unrealistic 18h magnitude stays.
- B. Wall-clock gap cap (cap the realized inter-tick gap at a few minutes, feed
  the ACD the UNCAPPED duration so state dynamics are unchanged). Looks surgical
  but is NOT: the gate measures REALIZED gaps, so capping lowers the very
  dispersion/ACF it validates - it breaks the same bands it appears to dodge, and
  would still require regenerating the golden stream and re-tuning to re-land the
  bands (perhaps impossible if the bands genuinely need the big gaps).
- C. Re-derive the duration model against a wall-clock-aware target (add a dwell
  constraint and refit): the only REAL fix, but a genuine offline-analysis project
  touching `analysis/build_fingerprint.py`, the committed fingerprint targets,
  `consts.rs`, the golden stream, and every coupled realism anchor (returns /
  abs-return ACF ride the same walk). High effort, high blast radius, and it
  partly relitigates whether the committed duration bands are the right targets.
- D. Split the concern: keep the realism gate on the PURE fitted dynamics (seed
  42, golden stream byte-identical) and apply a wall-clock dwell governor only on
  the SERVING path. Cheaper and keeps byte-identity, but then the validated tape
  is no longer the served tape - it breaks the premise that the committed
  fingerprint IS what ships, which is exactly where the byte-identical discipline
  and the usable-venue goal collide.

Current lean (discussion, not yet decided): the purpose framing pushes off "A
accept" toward a bounded fix, and treats the committed dispersion band as fair
game to loosen because it is a means (credibility) not the end (a venue you can
run against for a full day). B is a trap. The open decision is C vs D - i.e.
whether the tape that ships must stay the tape that `realism()` validates
(regenerate + re-anchor, the clean heavy path) or whether a serving-only governor
the gate does not see is acceptable (cheap, but validated tape != served tape).

Downstream of this decision: closing a LIVE in-progress bar window ON TIME on a
clock timer. That was deliberately not built, because it makes the
adapter-fabricated-bar venue more correct (against mogwai's
realistic-pathology-over-correctness stance) and the stall it would remove is
downstream of these deserts - a bar stalling on a quiet tape reads as the venue
going quiet. Revisit it with the D16 decision, not ahead of it.

---

## AD12. No dead-feed detection anywhere; a subscribed-but-silent feed is structurally invisible

`crates/mogwai-adapter/src/client.rs`, both transports. Severity medium,
confidence high.

The negative diagnostics are all in place now: the server emits a `ProtocolError`
frame on an unservable subscribe, the adapter's data drain warns on it instead of
swallowing it, and the poll loop self-heals after a server restart (a run of
consecutive failures re-fetches the clock and resets the cursor onto the fresh
tape). What is missing is the POSITIVE watchdog: no counter, no periodic "0 ticks
in N s for a subscribed symbol" heartbeat proving a subscribed feed is alive
rather than genuinely quiet, on either transport.

The WS idle timeout does not cover this. Its default `idle_timeout_ms` is 0
(disabled), and even when armed the idle clock resets on ANY application frame
(heartbeat, exec traffic), so a data-silent-but-frame-active socket never trips
it - deliberate, because that is what reproduces the 4255 case.

This is a real feature (a per-subscription liveness timer), flagged rather than
built. D16 makes it harder: on a desert tape a silent feed is often correct, so
the watchdog needs a threshold that distinguishes "the venue is asleep" from "the
subscription is dead", and that threshold depends on the D16 decision.

---

## AE2 / X3. No venue-truth source: lost-response desync is unrecoverable and any ack delay past ~25-30s becomes a guaranteed phantom reject

`crates/mogwai-adapter/src/client.rs` report generators and `query_order`;
mogwai-server routing. Severity high for the stated purpose of reconciliation
under havoc; confidence high on mechanism. These were filed separately but have
one root cause and one fix, so they are tracked together.

The desync half (AE2). When a POST times out or its 200 body fails to decode
AFTER the server processed the order, the adapter synthesizes `OrderRejected`;
the mirror flips to Rejected while the venue holds a live or filled order.
Likewise a fill dropped by `drop_prob` (or lost in a GoDark window, or to socket
death - the server delivers exec events per-session with no buffering and no
replay) never reaches the mirror. Because all three report generators rebuild
from the mirror - the same stream nautilus already saw - reconciliation can only
ever CONFIRM the client's wrong view, never heal it. Duplicates DO heal correctly
(`seen_trades` makes the mirror single-apply, so reports contradict the
double-counted nautilus state); drops cannot. Relatedly, the adapter never
re-pulls `GET /account` on an internal reconnect, so an account snapshot lost to
a blackout stays lost until the next fill-driven snapshot.

The inflight half (X3). `query_order` and the singular
`generate_order_status_report` sit at their trait defaults (log-only, `Ok(None)`),
and the server exposes no per-order status route. The nautilus live execution
manager gives an inflight order past `inflight_check_threshold_ms` up to
`inflight_max_retries` `QueryOrder` commands - which the adapter drops with "not
implemented" - and then synthesizes `OrderRejected("INFLIGHT_TIMEOUT")`.
Broadarrow arms threshold 5s / retries 5, so escalation lands at ~25-30s wall.
Past that, nautilus locally rejects the order, and the real delayed
`OrderAccepted`/`OrderFilled` then arrive on a Rejected order and are warn-dropped
as invalid transitions: the fill is lost at order level, and the venue/cache
position divergence is healed only by the position poll's synthesized
reconciliation fill.

That cliff clips mogwai's own advertised havoc band. `DelayAcks` validates up to
one HOUR, `MAX_LATENCY_NANOS` allows 60s of client-side delay, and GoDark goes up
to an hour - but anything beyond ~30s does not behave as a delay at all, it
exercises the consumer's brake. `reference/havoc.md` carries the operator note;
the root cause it names is this item.

The venue COULD answer these queries - for client-side latency and reorder holds
the adapter's own mirror is not blind, and the server has the engine truth. The
fix is a NEW WIRE PROTOCOL SURFACE (a `GET /orders` endpoint plus `query_order`
messages plus the adapter impl), which is why it is not being built
autonomously: it changes the protocol and needs sign-off.

---

## AD21. Client havoc corrupts adapter-fabricated bars at trade granularity

`crates/mogwai-adapter/src/client/data.rs`. Needs a deliberate decision, not a
fix.

Bars are built AFTER the `HavocFilter`, so a dup or drop of a single trade
silently reshapes OHLCV rather than duplicating or dropping a bar frame. Reordered
trades can also land in the wrong (later) window, since `update_bar_state` folds
any trade with `ts < close_ts` into the current window, including trades from
before that window started.

Defensible as modeling: a real venue's bars are derived from its trade feed, so
corrupting trades and deriving bars from the corrupted stream is arguably the
honest simulation. The alternative - corrupting bar frames as their own carrier -
models a venue that ships bars natively. Which one mogwai wants is the decision.

---

## X6. HttpOrders loses order-command ordering

`crates/mogwai-adapter/src/client.rs` `dispatch_order`. Severity low-medium;
confidence high that the race exists, medium that broadarrow hits it. The doc
half is done; the behavioral fix is the open option.

Each Submit/Modify/Cancel spawns a detached task doing its own POST, and nothing
sequences them (the `HttpQuota` mutex is not acquisition-ordered with respect to
spawn order). The WS path serializes through one cmd channel and has no such
race. So a submit immediately followed by a modify or cancel - broadarrow's
stale-order reconciliation cancel, for instance - can arrive at `/orders`
reversed, producing a spurious "unknown order" cancel-reject followed by an
accept or fill.

`reference/architecture.md` already scopes the byte-identical claim to
single-command semantics and discloses the race. The open option is the
behavioral fix: a per-client ordered queue in front of the spawned POSTs, which
trades the profile's deliberate fire-and-forget concurrency for ordering.

---

## F16. No end-to-end guard that startup reconciliation actually runs

`crates/mogwai-adapter/src/client.rs` exec client, against the nautilus
`ExecutionClient` trait defaults. Severity medium, confidence high.

The nautilus Rust `ExecutionClient` trait defaults are silent no-ops:
`generate_mass_status` returns `Ok(None)` and the three granular report generators
return empty vecs, and the live node consumes a `None` mass status by logging
"likely adapter error" and reconciling NOTHING - never erroring. (Python's
`LiveExecutionClient` base composes the three for you, so a Python adapter cannot
fall into this; the Rust trait does not, which is why mogwai hit it. That
Rust/Python default divergence is a consistent Rust-wide choice - all ~20 Rust
adapters hand-roll the composite - so it is a DRY nicety upstream, not a
correctness bug, and not worth a PR unless the Rust maintainers want the parity.)

mogwai implements `generate_mass_status` (composing the three, unit-tested by
`generate_mass_status_composes_the_three_report_sets`), so the instance is fixed.
Two exposures remain:

- No end-to-end guard. The unit test proves the method composes when called;
  nothing proves the live node actually reconciles a NON-EMPTY mass status at
  startup. If a future refactor drops the trait override, breaks the compose, or
  leaves one granular generator returning empty, startup reconciliation silently
  reverts to zero with only a warn line and no test goes red. Wanted: a
  reconciliation-path assertion (integration or smoke) that a seeded venue
  position or order actually reaches the node's reconciliation.
- The exposure is a CLASS, not one method. Every report path mogwai relies on -
  order status, fills, positions, and the `query_order` / singular
  order-status-report surfaces still unimplemented per AE2/X3 - shares the
  silent-degrade property. mogwai should treat "a nautilus report method we depend
  on returning its empty/None default" as a footgun to guard against explicitly,
  because nautilus will not surface the regression.

Also mirror-scoped: after a true process restart the mirror is empty, so mass
status is honest-but-empty. Healing that needs the AE2/X3 venue-truth source.

---

## X11. Single global venue ledger vs per-config account_id

mogwai-server has one engine account and no `account_id` concept anywhere; the
adapter stamps whatever `account_id` its config carries. Two workers or accounts
pointed at one server would each report the full venue net as their own position
and double-adopt it. Nothing refuses the second account.

A deployment constraint rather than a code bug, recorded because the failure is
silent. Refusing it properly means inventing a registration surface on a server
that today has no notion of who is connected.

---

## Open elsewhere (not mogwai work)

Two broadarrow decisions flagged by its developer, recorded here only so their
mogwai-side residues read as connected rather than orphaned:

- Enable the continuous open-order poll, closing the mid-run dropped-resting-cancel
  window for real venues, at REST-budget cost and needing a per-venue
  reconciliation override that does not exist. Inert against mogwai either way
  until AE2/X3 gives it something to call.
- Raise the inflight ceiling for mogwai only. No per-venue inflight config exists,
  and raising it globally would weaken the brake that protects real venues.
