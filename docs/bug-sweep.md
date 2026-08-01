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

Each item carries a VERDICT from a re-investigation against the current tree
(and against `research/nautilus_trader`, which is up to date), so what follows
is what the code says today rather than what the original sweep reported.

---

## D16. The seeded tape realizes multi-hour trade deserts, so the live region can be near-silent for most of a day

`crates/mogwai-data/src/generated/source.rs` (`next_duration_ns`) with the
committed `ACD_PERSISTENCE = 0.9935`, `ACD_FEEDBACK_SHARE = 0.08`, and Weibull
shape 0.6 innovations. Confidence high on mechanism (measured,
anchor-independent).

SEVERITY, RAISED: this was filed as a live-feed FIDELITY concern. Under the
forward-validation gate model (the venue now serves the multi-account fleet
described in `reference/architecture.md`'s account registry section) it is
closer to PIPELINE-BLOCKING. The gate exists to validate what a backtest
cannot - resting-order exit behavior -
and a strategy whose validation window lands in a multi-hour desert gets no
resting-order fills at all, so the very property being measured is never
exercised. It fails SILENTLY: the run completes and reports nothing wrong. At
batch scale that is a page of null results indistinguishable from strategies
that genuinely never triggered. Acceleration shortens the wall-clock cost of a
desert but not its effect - the strategy still sees a tape region with no
trades in it.

VERDICT: CONFIRMED, re-measured directly rather than inherited. `mogwai gen
--type bars --length 4d --interval 1h` on the default BTCUSDT profile at anchor
0 reproduces the reported shape almost exactly. Hours 0-5 run dense (806, 641,
501, 749, 864, 738 trades/hour - the expected ~7s cadence). Hours 6-25 then
collapse to 2-85 trades/hour, bottoming at 2 trades in a single hour, which is a
~20-hour near-silent stretch. A milder thin patch recurs around hours 30-36
(24-171/hour) before the tape recovers and stays dense (200-1200/hour) for the
remaining ~60 hours. No hour is completely empty, so this is near-silence rather
than a true gap - but an hour holding 2 trades means inter-trade gaps in the tens
of minutes, and nothing a real BTCUSD tape does.

The mean per-tick duration stays ~7-8s throughout, which is why a per-tick view
of the fit is blind to this: the deserts hide in the wall-clock projection, not
in the duration distribution.

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
tick-to-tick gaps over a DRAW of 2,000,000 ticks - at ~7s mean cadence that is
~160 days of tape, so the deserts sit squarely inside the gated sample and are
what push the measured dispersion (~190 on seed 42) and the duration-ACF up into
band. So the deserts are not an artifact bolted onto the fit - they ARE the
realized mechanism that achieves the committed duration realism. Remove or bound
them and the dispersion very likely falls through the 131.7 floor and the
duration-ACF signature collapses; `realism()` goes red.

That "realized" is verified, not assumed, and it is what makes option B a trap:
`measure` pushes `trade.ts_event` into `timestamps` and derives BOTH
`duration_dispersion_index` (`variance / mean`) and the duration ACF from
consecutive differences of that vector. The gate therefore reads exactly the
wall-clock gaps a cap would shorten - it never sees the generator's internal
`duration_s`.

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

VERDICT: CONFIRMED. No liveness timer, tick counter, or "0 ticks in N s" log
exists on either transport.

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

## AD21a. Dup/drop havoc reshapes fabricated bars instead of duplicating or dropping a bar frame

`crates/mogwai-adapter/src/client/data.rs`. Needs a deliberate decision, not a
fix. Split out of the original AD21, whose reorder half turned out to be a
different kind of finding entirely - see AD21b.

VERDICT: CONFIRMED. The drain runs each frame through `drain_havoc_anchored`
(the `HavocFilter`) and only the surviving frames reach `handle_market_message`,
which calls `emit_live_bars` - so bar construction is strictly downstream of
corruption on both the WS and poll paths. A dup or drop of a single trade
therefore silently reshapes OHLCV rather than duplicating or dropping a whole
bar.

The decision is which venue mogwai is modelling. Bars here are FABRICATED by the
adapter - the server never ships a bar - so corrupting the trade feed and
deriving bars from the corrupted stream is what a real client-side aggregator
on a lossy feed actually experiences, and is arguably the honest simulation. The
alternative, corrupting bar frames as their own carrier, models a venue that
ships bars natively, which mogwai is not.

Leaning accept-and-document, on the same principle that settled the account
staleness: mogwai injects faults and declines to repair them downstream.

---

## AD21b. Reorder havoc violates `fold_trade`'s documented ordering expectation

`crates/mogwai-data/src/bars.rs` against
`crates/mogwai-adapter/src/client/data.rs`.

VERDICT: CONFIRMED, and it was mis-filed as a modelling question. This half is
not "havoc makes odd bars" - it is a shared two-consumer API whose documented
contract one consumer breaks under a supported configuration.

`fold_trade` carried a PRECONDITION that `ts` is nondecreasing, asserting both
consumers satisfy it (the CLI via `GeneratedSource`'s monotone output, the
adapter because its live path drains an ascending source). The adapter's claim
is false whenever `reorder_prob > 0`: the `HavocFilter` reorders frames before
aggregation, so the adapter hands the core exactly the input the core called a
contract violation.

RESOLVED by making the documentation true rather than by changing behavior. The
core's response to an out-of-order trade is defined and bounded - the stale
trade folds into whatever window is open, nothing wedges, no bar is lost, and
the epoch-anchored grid is undisturbed for everything after it. So `fold_trade`
now documents an ORDERING EXPECTATION with a defined failure mode instead of a
precondition, names the adapter as a deliberate violator, and says explicitly
not to "fix" it with buffering, which would suppress an armed divergence.
`an_out_of_order_trade_folds_into_the_open_window_without_wedging` pins the
behavior so "defined and bounded" is a gate rather than an assertion.

Nothing further is open here; the entry stays only until AD21a is decided, since
the two share a call path.

---

## X6. HttpOrders loses order-command ordering

`crates/mogwai-adapter/src/client/exec.rs` `dispatch_order`.

DECIDED, NOT YET DONE. The race is real, but it is FIDELITY rather than a
defect, and the resolution is to document it as a property of the HTTP archetype
and close it. Agreed; the documentation work is deferred, so this entry stays
only to carry the decision until someone writes it up.

VERDICT: CONFIRMED as a race. `dispatch_order` hands each Submit/Modify/Cancel
to `get_runtime().spawn`, tracked for abort but not sequenced against its
siblings (the `HttpQuota` mutex is not acquisition-ordered with respect to spawn
order). So a submit immediately followed by a modify or cancel can arrive at
`/orders` reversed, producing a spurious unknown-order cancel-reject followed by
an accept or fill. The WS path serializes through one cmd channel and has no
such race.

WHY IT IS NOT A BUG - the finding that settles it: nautilus's own production
adapters dispatch exactly this way. The Binance futures execution client sends
every order command through `spawn_task("submit_order", ..)` /
`spawn_task("cancel_order", ..)` / `spawn_task("modify_order", ..)`, and
`spawn_task` is a bare `runtime.spawn` that only pushes the handle onto an abort
list - there is no cross-command sequencing anywhere in it. Real REST order
entry over independent HTTP requests genuinely has no ordering guarantee, and a
strategy firing a cancel on the heels of a submit can genuinely have them land
reversed against a real venue. Sequencing mogwai's dispatch would make its HTTP
profile MORE orderly than the venues it stands in for, which inverts the
project's premise: this is precisely the messy execution divergence mogwai
exists to surface, arriving for free.

Blast radius is also narrower than first filed. `TransportProfile::default()` is
`WsStreaming`, and broadarrow does not override it (its `transport_profile` is
an `Option` that keeps the adapter default when absent), so an HTTP order
profile is opt-in per scenario TOML. Anyone on this path chose the archetype
deliberately, and unordered dispatch is a defining property of that archetype.

WHAT REMAINS: a documentation pass only. `reference/architecture.md` already
scopes the byte-identical claim to single-command semantics and discloses the
race, but not the REASON - that it mirrors the production adapters and is
therefore a feature of the profile. Wanted: that rationale at `dispatch_order`
and in the architecture note, after which this entry is deleted. The per-client
ordered queue previously floated here is explicitly NOT wanted; it is recorded
as rejected so it is not re-proposed.

A genuinely separate feature, only if it is ever wanted in practice: an opt-in
ordering mode, so a strategy bug can be told apart from a transport race while
debugging. That is not a fix to this item.

---

## F16. No end-to-end guard that startup reconciliation actually runs

`crates/mogwai-adapter/src/client/exec.rs`, against the nautilus
`ExecutionClient` trait defaults. Severity medium, confidence high.

VERDICT: CONFIRMED on both sides. In the current `research/nautilus_trader`,
`ExecutionClient::generate_mass_status` still ends `log_not_implemented(..);
Ok(None)`, and the live node's `Ok(None)` arm still only warns "No mass status
available from {client_id} (likely adapter error when generating reports)" -
it does not error, and it reconciles nothing. On the mogwai side, no integration
or smoke test touches reconciliation at all: `tests/adapter_smoke.rs` covers
account seeding and submit-drives-exec-events, and neither goes near the report
path.

The nautilus Rust `ExecutionClient` trait defaults are silent no-ops:
`generate_mass_status` returns `Ok(None)` and the three granular report generators
return empty vecs, and the live node consumes a `None` mass status by logging
"likely adapter error" and reconciling NOTHING - never erroring. Python's
`LiveExecutionClient` base composes the three for you, so a Python adapter cannot
fall into this; the Rust trait does not, which is why mogwai hit it.

UPSTREAM (queued, not filed yet): a nautilus PR to give the Rust trait default
the same composing behavior as the Python base, framed as Rust/Python parity
rather than a bug fix. Everything it needs is already on the trait
(`client_id`/`account_id`/`venue`), `ExecutionMassStatus::new` plus the three
`add_*_reports` methods exist, and the `Generate*Reports` commands are
constructible from a default impl; the one open design question is where
`ts_init` comes from, since the trait exposes no clock. It is safe because all
~20 Rust adapters already override the method, so the default is unobserved.

CRITICAL: LANDING THAT PR DOES NOT CLOSE THIS ITEM. mogwai overrides
`generate_mass_status` itself, so a better trait default changes nothing here -
it protects the NEXT adapter author, not this repo. The local guard below is
required either way, and the two must not be confused for each other.

mogwai implements `generate_mass_status` (composing the three, unit-tested by
`generate_mass_status_composes_the_three_report_sets`), so the instance is fixed.
Two exposures remain:

- No end-to-end guard. The unit test proves the method composes when called;
  nothing proves a seeded venue actually produces a NON-EMPTY mass status through
  the real transport. If a future refactor drops the trait override, breaks the
  compose, or leaves one granular generator returning empty, startup
  reconciliation silently reverts to zero with only a warn line and no test goes
  red.

  DECIDED, NOT YET DONE - the shape agreed: extend the existing self-contained
  stub in `crates/mogwai-adapter/tests/common` (which speaks the protocol and
  already serves `/account`, but does not answer `QueryOrders`/`QueryFills`) to
  serve seeded orders and fills, then assert `generate_mass_status` returns all
  THREE report sets non-empty. Asserting each generator individually, not just
  the composite, is what turns the second bullet below from a written warning
  into a gate.

  Rejected as disproportionate: standing up a real `mogwai-server` from the
  adapter crate (which has no binary orchestration today), or a full nautilus
  `LiveNode`. The only thing a live node would catch beyond the stub approach is
  "nautilus stopped calling mass status at startup" - upstream behavior mogwai
  cannot fix, which would be a nautilus regression with its own tests.

  Known limitation to keep recorded rather than oversell: the stub guard proves
  the adapter WOULD answer correctly when asked. It cannot prove the node asks.
  That residual stays open after the guard lands.
- The exposure is a CLASS, not one method. Every report path mogwai relies on -
  order status, fills, positions, and the `query_order` and singular
  order-status-report surfaces - shares the silent-degrade property. mogwai
  should treat "a nautilus report method we depend on returning its empty/None
  default" as a footgun to guard against explicitly, because nautilus will not
  surface the regression. This got MORE important since the original finding, not
  less: the report paths now ARE the venue-truth query surface, so
  they carry real reconciliation weight rather than echoing a mirror, and a
  silent degrade costs more.

---

## Open elsewhere (not mogwai work)

Two broadarrow decisions flagged by its developer, recorded here only so their
mogwai-side residues read as connected rather than orphaned:

- Enable the continuous open-order poll, closing the mid-run dropped-resting-cancel
  window for real venues, at REST-budget cost and needing a per-venue
  reconciliation override that does not exist. This was recorded as inert against
  mogwai because there was nothing for it to call; that is no longer true - the
  venue-truth order query exists now, so mogwai would answer the poll. If
  broadarrow wants to evaluate the option, mogwai is ready to be the venue it
  tests against.
- Raise the inflight ceiling for mogwai only. Largely moot now: the ceiling was
  a problem because mogwai could not answer `QueryOrder` and every inflight order
  escalated to a synthesized timeout. It answers now, so the brake fires only
  when havoc actually withholds the reply - which is the behavior the brake is
  for. No per-venue inflight config exists, and raising it globally would weaken
  the protection real venues depend on.
