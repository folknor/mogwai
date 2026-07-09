# Bug sweep - consolidated findings

Six read-only hunt agents over the workspace, launched in two waves of three.
This document consolidates every finding as reported: deduped across agents
where two reports hit the same defect, but NOT filtered - low priority and nit
findings are recorded alongside the bugs. Severity and confidence are the
reporting agent's own judgment unless a fix batch has verified them.

This is a living tracking doc: verify-and-fix batches remove resolved entries
entirely (the record of what was fixed lives in git history), so everything
below is a CURRENT gap. Gaps in the ID numbering are fixed-and-removed items.

Wave 1 (complete): engine + protocol, data path, server.
Wave 2 (complete): adapter execution half, adapter data half, cross-boundary
contract audit against research/.
Fix batch 1 (complete): E1, E2, E6, E8/AE11, E12, E13, E14, D1, D2, D3, S1,
S2, S3, S4, S6, AD1, AD20 verified-and-fixed (every one confirmed real), plus
a lateral TradeId-length panic found and fixed in convert.rs.
Fix batch 2 (complete): E3, E15, AE1, AE3, AE4, AE5, AE7, AE12, F1, F2, F3,
S5, S8, S9, X1 verified-and-fixed (every one confirmed real).
Fix batch 3 (complete): AD2, AD3 (subsumes AD23), AD8, AD9, AD13, AD14, AD15,
X8, F7, D4, D5, D6 verified-and-fixed (every one confirmed real); a
non-normalized-flat-session test in mogwai-server that had encoded the D4
footgun was reworked to exercise genuine intraday shape.
Fix batch 4 (complete): E5, E7, D8, D9, D10, D12, S7, S10, S15, S16, S19,
S20, S21, S22b, S23, AD5, AD6, AD7, AD10, AD11, AD16, AD17, AD24, AD26, AE6,
AE8, AE9, AE10, AE18, AE19, F8, F11, F12, F13, F14 fixed; E4, S17 refuted;
E9, E10, D13, D14, D15, S18, AE13, AE14, AE15, AE16, AE17 resolved as
documented-deferrals; F15 was already fixed in an earlier batch. Remaining
open items are the design-scoped ones (AD4, AD12, AE2, X3), the flagged
follow-ups, and the doc-drift items the orchestrator owns.
Post-batch-4 (orchestrator solo): S14/D7 the unbounded checkpoint-index
memory fixed via geometric coarsening (a hard MAX_CHECKPOINTS ceiling,
byte-identity preserved); the reference-doc drift resolved (S11, S12, AD18,
X9, F17, S18); F18 assessed as no-autonomous-change. See "Found during fix
batches" and "Upstream candidates" at the bottom.
Mechanical batch 5 (complete): S10 residual fixed (the replay thread's
dead-seek and unknown-symbol ProtocolError diagnostics now ride an exec_tx
threaded through ReplaySpawn, so DelayAcks holds them); AD22 fixed
(date_to_unix_nanos saturates out-of-range datetimes at the axis bounds
instead of silently unbounding the request); AD25 fixed (ship_server_havoc
uses the configured request timeout like dispatch_order); F4's pin test added
(daemon startup failure asserted to land in the configured log). AD24 and
AD26 were already fixed in batch 4 but had stale entries in the nits list -
removed. X5 and X6 resolved by documentation: havoc.md now states
DuplicateNextFill is structurally inert against the nautilus stack, and
architecture.md scopes the byte-identical claim to single-command semantics
and discloses the HTTP inter-command ordering race. X6's behavioral fix
(sequencing HttpOrders dispatch) stays flagged as a design option.
Upstream follow-through (complete): the U1/U2 nautilus fixes verified merged
(LiveTimer is check-then-fire with an inclusive stop boundary; LiveClock's
timer_exists filters expired timers), so the mogwai clock's deliberately
bug-compatible AE14/AE17 mirrors were flipped to match the FIXED behavior:
check-then-fire in the MogwaiTimer loop, expiry-filtered timer_exists, and
fire_immediately on an at-now alert (mirroring LiveClock's set_time_alert_ns,
without which the new pre-fire stop check would swallow an adjusted-to-now
alert). Pin tests inverted; AE15's catch-up burst now includes the fire ON
the stop boundary. AE15/AE16 remain deliberate as documented.

IDs: E = engine/protocol, D = data path, S = server, AD = adapter data half,
AE = adapter exec half, X = cross-boundary seams, F = found during fix
batches. Wave-2 dedupe notes for surviving entries: the serial havoc-latency
throughput cap was reported independently by both adapter agents (recorded
once as AD4); the request_bars truncation by the data and seam agents (AD3);
the submit-before-dispatch OrderSubmitted by the exec and data agents (AE8);
the min-start_ts cursor drag by the data and exec agents (AD7).

---

## Engine + protocol (crates/mogwai-engine, crates/mogwai-protocol)

### Engine/protocol smells and nits

- E11. smell - `seen_client_order_ids` grows forever (documented as
  intentional for terminal-vs-unknown discrimination). Since fix batch 2 it is
  a map retaining a venue id per entry (the E3 fix), so the deliberate
  unbounded growth now carries slightly more per-entry weight.
- E16. nit (deferred) - protocol's `#[cfg(test)] mod tests` sits mid-file with
  `SubmitOrder`, `ServerMessage`, and the `control` module defined after it.
  Batch 4 assessed and deliberately skipped moving it: the block is ~1080
  lines, so relocating it would swamp real diffs for a pure cosmetic gain.
  Left in place on purpose.

(E9 dead defensive paths and E10 always-zero commission were resolved in
batch 4 as documented-deliberate: E9's unreachable-through-`process` warn
paths are kept as intentional defense-in-depth with comments saying so, and
the zero commission is documented as intentional with the sign-handling math
retained for correctness.)

### Engine/protocol verified-correct notes

Suspect areas that check out per the reporting agent: duplicate-fill ledger
stays single-counted (only the wire event is doubled); VWAP sign flips
(add/reduce/exact-flat/flip) correct apart from E1's overflow; FOK spending
the consumed partial is by design and documented; `DropNextAccountUpdate`
consumed only on the fill-driven path; floored partial + leaves sums to
quantity and stays on-grid; `take_armed` scan semantics correct and well
tested; SimClock saturation correct with honestly-documented precision limits;
serde round-trips and partial-payload defaults hold on both crates.

---

## Data path (crates/mogwai-data)

### Data-path smells and nits

- D11. nit - VolStorm's clamp lift touches the feedback path: `base_return`
  (the GARCH feedback) is clamped at the LIFTED cap, and sigma2's cap is
  lifted too, so under a storm the recursion state can differ from clean
  whenever the clean clamps would have bound. architecture.md's "recursions
  consume their un-modulated feedback" is strictly true only for the
  multiplier, not the clamp lift. Deliberate per in-code comments; doc
  tension only.

(D8 panicking constructors, D9 crate-doc drift, D10 UTF-8-truncates-CSV, and
D12 the byte-identity test's narrow comparison were all fixed in batch 4:
`try_new` / `try_new_with_session_profile` return `Result` alongside the
unchanged panicking constructors, a non-UTF-8 CSV line skips-with-warn, the
crate doc leads with `GeneratedSource`, and the test now compares size and
aggressor too. D13 lenient parse edges, D14 the seed-independent Buyer
opening, and D15 the untethered drift walk were assessed and documented as
deliberate - bounding any would break the committed golden stream for no
fidelity gain.)

### Data-path verified-correct notes

Per the reporting agent: MergeSource tie-break on equal ts is deterministic
(first minimum wins, MemorySource sort is stable); the `None` regime path is
genuinely byte-identical (no RNG consumed by any regime branch); checkpoint
clones are exact-state so seek-then-run has zero FP divergence; hour/dow
indices provably in range with the Sun=0 convention matching the fingerprint;
ChaCha12Rng clone preserves stream position; conditional RNG draws are
replay-stable; the data crate's `decimal_from_f64` saturating variant
divergence from the protocol's zeroing variant is deliberate and documented.

---

## Server (crates/mogwai-server)

(S10 is now fully closed: batch 4 routed the two `handle_socket` diagnostics
through the exec pump, and batch 5 threaded an exec sender through
`ReplaySpawn` so the replay thread's dead-seek and unknown-symbol diagnostics
ride it too. S11 the HTTP-carrier exemption from the temporal divergences and S12 the
config.md/cli.md omissions were resolved by documentation: havoc.md now states
the writer-only / WS-only scope of DelayAcks/GoDark/StallData as a deliberate
connection-scoped behavior and an operator trap, config.md gained the
`backfill_horizon_ns` row and the deny-unknown-fields caveat, and cli.md's man
topic list gained `clock`.)

### Server smells and nits

- S13. smell - One global checkpoint mutex serializes all symbols and holds
  through ~100 ms extensions: `positioned_generator` keeps the
  `checkpoint_store()` guard across `extend_to` (up to 190k synthesized
  ticks). Every live subscribe, seeked /trades, and price-less market order
  for ANY symbol queues behind it. Within stated per-call budget; bursty
  concurrency stacks.
- S17. nit (refuted) - Divergence atomics are all Relaxed. Batch 4 refuted
  this as a defect: the three atomics are independent with no cross-location
  invariant, and the arm and the order arrive over separate connections, so
  Release/Acquire would establish a happens-before nothing consumes. Left
  Relaxed on purpose.
- S18. nit (resolved) - Re-arming GoDark/StallData replaces the window (a
  smaller ms SHORTENS an in-flight blackout). Documented as deliberate
  store-not-extend semantics in code (batch 4) and in havoc.md.
- S22a. nit (flagged) - A subscribe of N symbols spawns N OS threads,
  client-controlled and unbounded. Batch 4 fixed the S22b half (unknown
  symbols are pre-filtered and no longer spawn a throwaway thread or leave a
  dead `replays` entry); the unbounded-thread-per-symbol shape remains and
  needs a concurrency-cap / behavior decision, so it is flagged rather than
  fixed.

(S15 silent panicking replay thread, S16 pre-synthesis timestamp sampling,
S19 the BoundedSeek off-by-one, S20 the missing deny_unknown_fields, S21 the
heartbeat cadence edges, and S23 the flat nested-list rendering were all
fixed in batch 4. S20 residual: serde cannot combine deny_unknown_fields with
the flattened InstrumentDef, so a typo'd instrument sub-field is still
tolerated - only top-level knobs are guarded.)

### Server verified-correct notes

Per the reporting agent: ClearDivergences clears all three atomics and
deliberately not the engine queue; heartbeat survives StallData and dies under
GoDark; /orders and /ws share process_order_cmd byte-identically; the engine
mutex is never held across an await; window_until_ns /
sim_duration_from_millis saturate; the two-knob-trap rejection matches
clock.md; the runtime is built after fork and the parent's mem::forget(lock)
is correct for the shared flock; normalize_limit / limit=0 /
inclusive-cursor semantics match the adapter's overlap-and-skip contract; the
NTP-immune anchor pairing in spawn_replay is sound.

---

## Cross-wave thread resolutions

- E3 (venue_order_id presence rule): FIXED in fix batch 2 - the engine now
  retains venue ids for terminal orders and the wire contract's presence rule
  holds; the adapter's fallback (old AE7) is aligned on both reject paths as
  defense in depth.
- E8 / AE11 (zero-initial backoff spin): confirmed and FIXED in fix batch 1
  at the validator; lifecycle comments trued up in batch 2 (old F2).
- D1 (exact-ts seek off-by-one): confirmed and FIXED in fix batch 1; the
  adapter-side rewind guard (old AD8) was added in fix batch 3.
- D3 (ReopenGap at_ts at or before the anchor): FIXED in fix batch 1 at the
  generator (consumed at construction with a warning, byte-identical to a
  clean run). Whether the server should ALSO reject it loudly at the API
  boundary is still open as a nice-to-have.
- S2 (resubscribe duplicate window): server half FIXED in fix batch 1
  (resume floor read after quiesce). The adapter-side stale-start_ts
  duplication paths remain - see AD5, AD7 - and the WS drain still has zero
  dedup.
- S5 (silent zero-frame subscription): server half FIXED in fix batch 2
  (warn plus ProtocolError frame on unservable subscribes), adapter half
  FIXED in fix batch 3 (the data drain now warns on ProtocolError instead of
  swallowing it, old X8). AD12 remains open only for the POSITIVE watchdog
  (no counter / heartbeat proving a subscribed-but-silent feed is alive).
- S11 (HTTP carriers exempt from temporal divergences): CONFIRMED at the seam,
  undocumented.

---

## Adapter, data half (crates/mogwai-adapter: client.rs data side, convert.rs, config.rs, factories.rs)

### AD4. bug - Serial per-message havoc sleep caps the drain at ~33 msg/s and lets the inbound queue grow unboundedly

`client.rs` `dispatch_havoc` / `sleep_havoc_delay`; `lifecycle.rs`
`run_ws_connection` handler await; affects the WS drain, the poll drain, and
the exec drain alike. Severity medium-high (data-path fidelity), confidence
high on mechanism, medium on how often real runs exceed 33 msg/s. (Reported
independently by both adapter agents.)

The mandatory 30 ms BASELINE_LATENCY is realized as an inline
`tokio::time::sleep` per message, awaited serially inside the single
reader/poll loop (the handler holds the shared filter mutex across the
sleep). Latency thus behaves as inter-message SPACING, not pipeline delay:
throughput ceiling ~1/30ms ~33 ticks/s on the sim axis. A burst above that
(VolStorm, catch-up backfill after reconnect, a 1000-trade poll page = 30+ s
to drain) backs up in the unbounded `in_rx` channel; queueing delay compounds
far beyond the modeled 30 ms and memory grows without bound. A real network
delays every frame ~30 ms in parallel at full throughput. While the WS select
loop sleeps, Ping replies, idle-reset processing, and outbound
commands/heartbeats are head-of-line blocked behind the backlog. At high
`speed` the wall sleep compresses and this vanishes; at speed 1 with an
active tape it materially distorts delivery timing.

### AD12. gap - No dead-feed detection anywhere; a zero-frame subscription is structurally invisible

`client.rs`, both transports. Severity medium, confidence high.

The server used to be able to ack a subscribe and stream zero frames (old
S5); since fix batch 2 it emits a ProtocolError frame on an unservable
subscribe, so the WS wire now carries the diagnostic. But the adapter's data
drain SWALLOWS ProtocolError (X8), so nothing downstream sees it yet. On the
WS path the default ConnHavoc has idle_timeout_ms = 0 (disabled), and even
when armed, the idle clock resets on ANY application frame (heartbeat, exec
traffic), so a data-silent-but-frame-active socket never trips it -
deliberate for the 4255 reproduction, but it means the CLEAN default has no
watchdog, no counter, no periodic "0 ticks in N s for subscribed symbol" log.
Batch 3 fixed X8 (the drain warns on ProtocolError) and batch 4 fixed AD6
(the poll loop now self-heals after a server restart: a run of consecutive
failures re-fetches the clock and resets the cursor onto the fresh tape). So
the diagnostics reach the log and a wedged poll recovers. What remains open
is the POSITIVE watchdog: there is still no counter or "0 ticks in N s for a
subscribed symbol" heartbeat proving a subscribed-but-silent feed is alive
(vs a genuinely quiet market) on either transport. That is the residual
AD12 - a real feature (a per-subscription liveness timer), flagged rather
than built.

(AD18 the architecture.md instruments-subscription drift was resolved: the doc
now states instrument subscriptions are one-shot fetch-and-emit and NOT on the
refcount table, which is correct for a static venue.)

### AD19. gap (downgraded) - Live in-progress bars still have no timer-based close

`client.rs` `update_bar_state`. Severity low-medium, design-adjacent.

Batch 4 built the safe half: on the final `unsubscribe_bars`, a window that
has already closed (close_ts <= sim-now) but was withheld for lack of a later
trade is now flushed instead of discarded. RESIDUAL (flagged, not built): a
clock timer that closes a LIVE in-progress window on time is a real feature,
and a `stop()`-teardown flush touches the delicate stop/reset/dispose path -
both left unbuilt. So under a thin tape or a halt, a live window's bar is
still emitted late (on the next trade) or, for the genuinely in-progress
window at teardown, dropped. Arguably realistic venue behavior; broadarrow
should know bars can stall on quiet tape.

### Adapter data-half smells and nits

- AD21. smell - Client havoc corrupts adapter-fabricated bars at trade
  granularity: bars are built AFTER the HavocFilter, so dup/drop of a single
  trade silently reshapes OHLCV instead of duplicating/dropping a bar frame,
  and reordered trades can land in the wrong (later) window since
  update_bar_state folds any ts < close_ts trade into the current window,
  including trades before the window's start. Defensible modeling choice;
  worth a deliberate decision.
(AD22 and AD25 were fixed in batch 5: date_to_unix_nanos saturates
out-of-range datetimes at the axis bounds with a unit test pinning both
sides, and ship_server_havoc takes the configured request timeout. AD24 and
AD26 turned out already fixed in batch 4 - their nit entries here were stale
and are removed.)

### Adapter data-half verified-correct notes

Per the reporting agent: `PollCursor::unseen_from_batch` is correct for the
intended stable-ascending server contract, including cap-mid-timestamp and
empty pages (remaining failure modes at the contract edges: 1000+ trades
sharing one ts wedges the cursor forever, inherent to ts-cursors and
undetected; server restart AD6; duplicates AD5/AD7 - the rewind guard was
added in fix batch 3). Tick and volume bar specs are refused with ensure!
(good); bar ts_event = window close, correct. Convert precision uses rounding (not truncation) via nautilus
f64_to_fixed, monotonic so Bar OHLC invariants survive re-rounding;
magnitude/precision overflows handled via new_checked for
Price/Quantity/Money. (The sweep's claim that the zero-size trade was "the
one hole" here was wrong: fix batch 1 found and fixed a second panicking
constructor in the same expression - the synthetic trade id overflowed
nautilus's 36-char TradeId cap for realistic ticks.) The ws-to-http and
wss-to-https derivation is correct (ports, paths, trailing slashes); config
validation is solid now that ws_url trims. Factories: downcast failures
clean, both named MOGWAI, create-twice yields independent clients, no
blocking calls in create.

---

## Adapter, exec half (crates/mogwai-adapter: client.rs exec side, lifecycle.rs, clock.rs)

### AE2. gap - Lost-response desync is unrecoverable: reconciliation reports rebuild from the same corrupted client-side mirror, and the venue has no order-status query surface

`client.rs` report generators; mogwai-server routing (no GET /orders).
Severity high for the stated purpose of reconciliation under havoc;
the mirror-based design is documented, so possibly a known accepted limit.
Confidence high on mechanism. Cross-links X3.

When a POST times out or its 200 body fails to decode AFTER the server
processed the order, the adapter synthesizes OrderRejected; the mirror flips
to Rejected while the venue holds a live/filled order. Likewise a fill
dropped by drop_prob (or lost in a GoDark window / socket death - the server
delivers exec events per-session via that session's exec_tx, no buffering, no
replay) never reaches the mirror. Because all three report generators rebuild
from the mirror - the same stream nautilus already saw - reconciliation can
only ever CONFIRM the client's wrong view, never heal it. Duplicates DO heal
correctly (seen_trades makes the mirror single-apply, so reports contradict
the double-counted nautilus state); drops cannot. Fixing this needs a
venue-truth source: the server exposes only POST /orders. Also: the adapter
never re-pulls GET /account on an internal reconnect, so an account snapshot
lost to a blackout stays lost until the next fill-driven snapshot.

(AE6 unbounded ExecState, AE8 submit-before-dispatch, AE9 WS cancel/modify
errors surfacing nowhere, AE10 the lookback hiding long-quiet open orders,
and the AE18 silent unrepresentable-currency drop were all fixed in batch 4.
AE13 the HttpQuota-mutex-across-sleep and inconsistent accounting was
documented as its deliberate metered-vs-exempt contract - the clock-fetch
bypass is an unavoidable chicken-and-egg; routing ship_server_havoc through
the quota is a client.rs follow-up, F18. AE14 and AE17 were bug-compatible
mirrors of nautilus LiveTimer/LiveClock tracked upstream as U1/U2; the
upstream fixes are merged and the mirrors flipped to match the FIXED
behavior. AE15 past-start catch-up remains a deliberate deviation from
LiveTimer and AE16 the senderless clock is documented as reachable only off
the canonical wiring path - both with pin tests. The AD17/AE19
spawned-task-tracking twin was fixed in batch 4: request_ and dispatch tasks
are now tracked and aborted on stop.)

### Adapter exec-half verified-correct notes

Per the reporting agent: request_timeout_secs == 0 maps to the 30 s default,
scaled and floored exactly as clock.md documents. The speed-0 sleep wedge is
unreachable (server build_sim_clock rejects it; adapter only gets clocks from
/clock or identity). DuplicateNextFill vs the mirror: seen_trades dedup
applies each economic fill exactly once while forwarding the duplicate wire
event - correct on both counts. The OrderCancelRejected FSM assumption
(nautilus restores pre-pending status) holds, so leaving the mirror status
untouched is right. HttpQuota ceil-divide, jitter seeding, backoff
clamp/no-clamp-at-zero-max, and sim scaling all match havoc.md and are
pinned by tests. The reader thread never touches the nautilus cache; only
connect's await_account_registered does, on the runner's thread. Account
seeding on connect (fetch, dispatch, poll the cache with 5s/10ms wall
bounds, 404-vs-other split) is implemented as documented. Idle-timeout
contract confirmed: reset_idle fires only on Text/Binary application frames;
Ping/Pong do not reset it, preserving the StallData/4255 reproduction.
Data-socket resubscription cursor semantics check out: dropped-by-havoc ticks
never advance the cursor (advance happens post-filter), delivered ticks are
excluded via ts_event + 1.

---

## Cross-boundary seams (wire protocol, nautilus FSM/traits, broadarrow expectations)

### X1. (fixed in batch 2 - generate_mass_status now composes the three report sets. Residuals: the mirror is in-memory, so after a true process restart mass status is honest-but-empty - healing that needs the AE2/X3 venue-truth source - and broadarrow's disabled open-order poll is tracked as U4.)

### X2. (merged into AD3 - request_bars unit mismatch and single-page truncation)

### X3. gap - No order-status query surface at all: query_order / generate_order_status_report unimplemented and the server has no endpoint, so any ack delay past ~25-30 s becomes a guaranteed phantom local reject

Adapter/server: query_order and the singular generate_order_status_report are
left at trait defaults (log-only / Ok(None)); mogwai-server exposes no
per-order status route. Nautilus: the live execution manager gives inflight
orders past inflight_check_threshold_ms up to inflight_max_retries QueryOrder
commands (which the adapter drops with "not implemented"), then synthesizes
OrderRejected("INFLIGHT_TIMEOUT"). Broadarrow arms threshold 5 s / retries 5,
so escalation lands at ~25-30 s wall. Severity medium (partly intended havoc,
but the escalation path is unanswerable by design); confidence high on
mechanics. Cross-links AE2.

Failure scenario: DelayAcks is validated up to one HOUR, MAX_LATENCY_NANOS
allows 60 s client-side delay, GoDark up to an hour. Anything beyond ~30 s
does not behave as a delay: nautilus locally rejects the order, then the real
delayed OrderAccepted/OrderFilled arrive on a Rejected order and are
warn-dropped as invalid transitions - the fill is lost at order level and the
venue/cache position divergence is only healed by the position poll's
synthesized reconciliation fill. The venue COULD answer these queries from
the adapter's own mirror (for client-side HavocLatency/reorder holds the
mirror is not blind). No doc mentions the ~30 s cliff.

### X4. (merged into AE3 - mirror terminal-state regression)

### X5. (resolved by documentation in batch 5 - havoc.md's DuplicateNextFill entry now states the duplicate is structurally inert against the nautilus stack, exercising the dedup path and its logging only. The open design OPTION remains: distinct trade_ids on the duplicate would be a genuine double-fill/overfill divergence - the interesting path under broadarrow's allow_overfills=false - but that is a new divergence, not a doc fix.)

### X6. smell (doc half resolved) - HttpOrders profile loses order-command ordering

`client.rs` `dispatch_order`: each Submit/Modify/Cancel spawns a detached
task doing its own POST; nothing sequences them (the HttpQuota mutex is not
acquisition-ordered w.r.t. spawn order). The WS path serializes through one
cmd channel. Severity low-medium; confidence high that the race exists,
medium that broadarrow hits it.

Failure scenario: submit immediately followed by modify/cancel (e.g.
broadarrow's stale-order reconciliation cancel) can arrive at /orders
reversed - a spurious "unknown order" cancel-reject followed by an
accept/fill. Batch 5 trued up architecture.md: the byte-identical claim is
now scoped to single-command semantics and the inter-command ordering race is
disclosed. The BEHAVIORAL fix - sequencing HttpOrders dispatch (a per-client
ordered queue in front of the spawned POSTs) - remains flagged as a design
option; it trades the profile's deliberate fire-and-forget concurrency for
ordering.

### X7. (merged into S11 - temporal divergences do not apply to the HTTP carriers; confirmed and expanded there)

### X9. (resolved - architecture.md now states the exec client builds at whatever OmsType the config carries, that the Netting default is a knob not the operating reality, and that broadarrow runs it at Hedging.)

### X10. (merged into AE7 - asymmetric venue-id fallback between the two reject paths; see also E3)

### X11. observation - Single global venue ledger vs per-config account_id

mogwai-server has one engine account; the adapter stamps whatever account_id
its config carries. Two workers/accounts pointed at one server would each
report the full venue net as their own position and double-adopt it.
Deployment constraint rather than code bug; nothing refuses the second
account.

### Cross-boundary verified-correct notes

Per the reporting agent: event-constructor signature parity between the
adapter's call sites and the research nautilus copy checks out exactly
(OrderUpdated 15-arg form, OrderCancelRejected, Accepted/Rejected/Canceled/
Filled, OrderStatusReport, PositionStatusReport, FillReport) - no
research-vs-usage drift, and the workspace builds against the sibling so live
drift is impossible by construction. Decimal wire format: mogwai's
serde-with-str and nautilus's serde-with-float are both the opt-in module
variants that do not change the global default, so feature unification cannot
skew the wire. HavocSpec cross-repo compat is a compile-time guarantee
(broadarrow consumes mogwai_protocol via path dep). The market-order price
seam is bridged by the server stamping a price on BOTH carriers before the
engine, failing loudly if synthesis fails; the adapter correctly never stamps
(staleness of the stamp is S1). await_account_registered works during
connect() because the runner applies Account events immediately while
buffering order events, and instruments are flushed into the cache before
exec connect - matching emit_seeded_instruments' assumption. The mirror's
flat-position convention matches nautilus reconcile_position_reports exactly
(absent/zero = flat; cached-but-unreported = synthesized closing fill;
venue-nonzero-uncached = EXTERNAL). The order-report lookback filter matches
the manager's own ts_last-cutoff logic (no false missing-at-venue cancels
today; see AE10). FSM legality of every in-order engine sequence checks out,
including Submitted-to-Filled with a dropped accept, the IOC
partial-then-cancel pair, pending-restore on both reject kinds, and the
(Canceled, Filled) race; zero-qty fills are guarded engine-side.

---

## Found during fix batches (new, unresolved)

(F4 was closed in batch 5: `daemon_startup_failure_lands_in_the_log` in
crates/mogwai-server/tests/daemon.rs daemonizes with a malformed config and
asserts the failure lands in the configured log. The narrow
pre-init_logging window remains unlogged by nature and is documented in the
test's comment.)

### F5. note - Engine ledger saturation is sticky by design

The batch-1 E1 fix saturates accumulations instead of panicking, with
once-per-key warns. After the first clip, later opposite-direction fills move
off the boundary from a wrong base, so the ledger stays wrong (loudly).
Accepted trade-off; recorded so nobody mistakes the warn for a self-healing
state.

### F6. note - LiquidityDrought stretches remain once-sampled

The batch-1 D2 fix integrates the session envelope hour-by-hour only below
the closed-window gate (arrival multiplier < 0.01). A drought's thin_factor
stretch on an open-hour gap is still a once-sampled multiplier, now capped at
366 days. Deliberate (thin tape is a venue-wide divergence, not a session
curve); recorded as behavior, not a bug.

### F9. gap - Adapter clock skew against the /trades sim-now refusal (now visible, self-heals)

Cross-crate (server 422 vs adapter poll loop). Found by the batch-2 server
agent. Severity low (same-machine deployments safe), confidence medium.

If the adapter's sim-now anchor ever runs ahead of the server's (cross-machine
NTP skew), its poll `start` can trip the start-beyond-sim-now 422. Batch 3
made it log rate-limited (visible, not silent), and batch 4's AD6 self-heal
now re-fetches the clock and resets the cursor after a run of failures - so
this case now recovers rather than staying dead. Kept as a note that the
underlying cross-machine skew is a deployment consideration.

### F10. note - The OrderRejected arm still overwrites mirror status unconditionally

`client.rs`. Deliberately left by the batch-2 client agent: the engine emits
Rejected as an order's sole lifecycle event so no reorder pair exists; the
only reachable overwrite is the HTTP synthesized-reject-after-processed case,
which is AE2's documented unrecoverable-desync class. Recorded so the
asymmetry with the new terminal-state guards reads as a decision, not an
oversight.

### F16. gap - mogwai has no end-to-end guard that startup reconciliation actually runs; the nautilus report contract degrades silently

`mogwai-adapter/src/client.rs` exec client, against nautilus's
`ExecutionClient` trait defaults. Severity medium, confidence high. The
mogwai-side residue of the withdrawn U3 (see the upstream section).

The nautilus Rust `ExecutionClient` trait defaults are silent no-ops:
`generate_mass_status` returns `Ok(None)` and the three granular report
generators return empty vecs, and the live node consumes a `None` mass status
by logging "likely adapter error" and reconciling NOTHING - never erroring.
(Python's `LiveExecutionClient` base composes the three for you, so a Python
adapter cannot fall into this; the Rust trait does not, which is why mogwai
hit it.) X1 fixed the instance by implementing `generate_mass_status`
(composing the three, unit-tested by
`generate_mass_status_composes_the_three_report_sets`), but two exposures
remain:

- No end-to-end guard. The unit test proves the method composes when called;
  nothing proves the live node actually reconciles a NON-EMPTY mass status at
  startup. If a future refactor drops the trait override, breaks the compose,
  or leaves one granular generator returning empty, startup reconciliation
  silently reverts to zero with only a warn line - no test goes red.
  Wanted: a reconciliation-path assertion (integration or smoke) that a
  seeded venue position/order actually reaches the node's reconciliation.
- The exposure is a CLASS, not one method. Every report path mogwai relies on
  (order status, fills, positions, and the query_order / singular
  order-status-report surfaces still unimplemented per X3) shares the
  silent-degrade property. mogwai should treat "a nautilus report method we
  depend on returning its empty/None default" as a footgun to guard against
  explicitly, because nautilus will not surface the regression.

### F17. (resolved - havoc.md's temporal-divergence section now carries the operator note: any nautilus consumer's inflight timeout converts a delay window longer than its threshold into a local reject, so a multi-minute DelayAcks/GoDark/latency window exercises the consumer's brake, not mogwai's delay path. Framed generally, without baking in broadarrow's constant. The root cause that mogwai cannot resolve the inflight instead is X3.)

### F18. assessed - three cross-file follow-ups the batch-4 agents flagged; none warrant an autonomous change

- Adopt `try_new` in the server (D8's cross-file half): ASSESSED, not needed.
  The server validates `GeneratorScalars`, `SessionProfile`, and the
  instrument def at config load (main.rs ~158/178/187), so the generator
  constructor's `.expect` is already unreachable via the config path. Plumbing
  `Result` through the request path (`positioned_generator` ->
  `bounded_generator` -> the live/history builders) would be churn for a panic
  that cannot fire. `try_new` now exists for any FUTURE caller that builds a
  generator from unvalidated input.
- Wire `clear_armed()` into the server's `ClearDivergences` handler (E5 flag):
  NOT done autonomously. `ClearDivergences`'s documented wire contract is that
  it clears the server-owned temporal windows and deliberately does NOT flush
  engine-side single-shots. Wiring `clear_armed()` in would change that
  documented contract - a decision, not a mechanical follow-up.
- Route `ship_server_havoc` through the `HttpQuota` (AE13 flag): NOT done
  autonomously. Whether a connect-time control-plane loop should be throttled
  by the venue's data-request quota is a genuine judgment call (they model
  different concerns), the default quota is unlimited so there is no current
  effect, and the lifecycle agent documented the bypass as deliberate. Left
  as-is.

### F19. gap - larger design/feature items deferred out of the mechanical sweep

Recorded so they are not mistaken for oversights. Each needs a design
decision or a non-trivial build, so they were flagged rather than forced:

- AD4: the serial per-message havoc-latency sleep behaves as a throughput cap
  rather than pipelined delay. A faithful fix pipelines the delay across the
  drain without serializing throughput - a drain rearchitecture spanning
  client.rs and lifecycle.rs.
- AD12: a POSITIVE dead-feed watchdog (a per-subscription liveness timer /
  "0 ticks in N s" heartbeat). Diagnostics now reach the log; what is missing
  is a positive proof-of-life. A real feature.
- AD19 residual: a clock timer that closes a live in-progress bar window on
  time, plus a stop()-teardown flush.
- S22a: the unbounded-OS-thread-per-subscribed-symbol shape needs a
  concurrency-cap / behavior decision.
- AE2 / X3: the venue-truth source (an order-status query surface). This is a
  NEW WIRE PROTOCOL SURFACE (a `GET /orders` endpoint + `query_order` messages
  + adapter impl) and is NOT being built autonomously - it needs sign-off
  because it changes the protocol. See X3 for the full shape and options.

---

## Upstream candidates (dependencies we have commit access to)

Findings whose root cause or better fix lives in a dependency. Each needs a
written report (or a direct fix) in the dependency's own repo; the research/
copies are read-only mirrors, the sibling checkouts are the real trees.

Status: all five triaged and closed. U1, U2 fixed upstream and verified
merged (the live timer moved to common/src/live/timer.rs and is
check-then-fire with an inclusive stop boundary; LiveClock's timer_exists in
common/src/live/clock.rs filters expired timers consistently with
timer_names/timer_count, both with upstream pin tests) - mogwai's AE14/AE17
mirrors were flipped to match, so nothing remains tracked here. U3 withdrawn
(not an upstream bug; the fix was mogwai-side X1; residue F16). U4, U5
verified by the broadarrow developer as deliberate C5d decisions and
resolved by documentation on the broadarrow side, no behavior change; their
mogwai-side residues are X3 (both) and F17 (U5). The two broadarrow BEHAVIORAL
options below (enable the continuous poll; per-venue inflight ceiling) remain
open as broadarrow decisions, not mogwai work.

### U3. WITHDRAWN - "the node should fall back to the three generators" is not an upstream bug; the fix was correctly mogwai-side (X1)

Original framing (node should fall back on `None`) is refuted by the code:

- The Rust node's `None` arm (`crates/live/src/node/mod.rs` ~668) and the
  Python engine's `None` arm (`nautilus_trader/live/execution_engine.py`
  ~1721) are IDENTICAL: warn "likely adapter error", skip, no fallback. A
  Rust engine-level fallback would break the Rust/Python parity the project
  holds sacred. So the ENGINE handling is correct on both sides.
- `generate_mass_status` composed from the three report generators is the
  canonical adapter contract, not a workaround: every Rust adapter
  (kraken/spot ~1256, binance, IB, ...) implements it as
  `ExecutionMassStatus::new` + `add_order_reports`/`add_fill_reports`/
  `add_position_reports`. mogwai's X1 fix does exactly this and is in place
  (`client.rs` ~2734, test `generate_mass_status_composes_the_three_report_sets`).

So the symptom was mogwai-side and is fixed. Keeping the entry, downgraded,
only to record one genuine-but-deliberate parity nuance found while checking:

- The DEFAULTS diverge, not the engines. Python's `LiveExecutionClient` base
  class ships a CONCRETE `generate_mass_status`
  (`execution_client.py` ~440-514) that composes the three sub-generators for
  you, so a Python adapter implementing only the granular generators gets a
  working composite for free. The Rust `ExecutionClient` trait default
  (`crates/common/src/clients/execution.rs` ~288) instead returns `Ok(None)`,
  and the Rust live-layer client just delegates (no composition). That is why
  mogwai (Rust) hit the silent skip where an equivalent Python adapter would
  not.
- This is a real Rust-vs-Python default-composition gap, but it is a
  CONSISTENT Rust-wide choice - all ~20 Rust adapters hand-roll the composite
  - so "fix" upstream would be a DRY/parity nicety (give the Rust trait or
  live client a default that composes the three), not a correctness bug. Not
  worth a PR unless the Rust maintainers want the parity; noted so the
  reasoning is not re-litigated.

The mogwai-side residue of this investigation is tracked as F16.

### U4. RESOLVED (deliberate broadarrow decision, documented) - the disabled continuous open-order poll

Verified by the broadarrow developer against `crates/run-prep/src/lib.rs`:
`open_check_interval_secs: None` is a deliberate C5d decision, not an
oversight, and the "dead code" framing was imprecise:

- The three report generators DO run at startup (mass-status reconcile); only
  the CONTINUOUS mid-run open-order poll is off.
- Resting orders (limit/stop/bracket/trailing) are reconciled every bar by
  `core::plan_stale_cancels` (piners' shadow book vs the Nautilus cache, with
  bracket/partial-fill/trail-rearm handling in `reconcile.rs`), and
  order-level state is carried by the lifecycle callbacks
  (`on_order_filled`/`rejected`/`denied`/`modify_rejected`). So order-level
  reconciliation is not absent, it is done by a different mechanism.
- Genuine residual (now disclosed in broadarrow docs): an out-of-band resting-
  order cancel is dropped mid-run and heals only at reconnect/restart.

Resolution taken: documentation only - the stale rationale comment was
rewritten and the position-only-continuous posture added to broadarrow's
`reference/execution.md`. No behavior change.

Mogwai-side residue: enabling the continuous poll would be INERT against
mogwai anyway, because mogwai exposes no order-status query surface for it to
call - that is X3, which this makes doubly-motivated (it is the mogwai-side
blocker for participating in continuous order-level reconciliation at all).

Open broadarrow OPTION (not a bug, flagged by the developer): enable the
continuous open-order poll to close the mid-run dropped-resting-cancel window
for real venues (Binance/Kraken/Bybit), at REST-budget cost and needing a
per-venue reconciliation override that does not exist today. A decision for
broadarrow, not mogwai.

### U5. RESOLVED (deliberate broadarrow decision, documented) - inflight-timeout brake clips mogwai's long-delay havoc band

Verified by the broadarrow developer against the Nautilus manager: threshold
5 s, re-query throttled to ~5 s spacing, 5 retries -> INFLIGHT_TIMEOUT
synthesized at ~25 s in-flight. This is a CORRECT safety brake for a silent
real venue; it is not a bug on either side. The only gap was that it was
undocumented that the brake clips mogwai's advertised hour-long
DelayAcks/GoDark and 60 s latency band.

Resolution taken: documentation only - broadarrow's `reference/mogwai.md`
gained an operator note (arm ack-delay havoc below ~25 s to exercise the
delay path; longer windows only exercise the brake). No behavior change.

Mogwai-side residue: the root reason the brake fires is X3 - mogwai cannot
answer the manager's `QueryOrder` (no order-status query surface), so an
inflight order can only escalate to the synthesized timeout, never resolve.
See also F17 for the mogwai-doc side of this.

Open broadarrow OPTION (not a bug, flagged by the developer): raise the
inflight ceiling for mogwai only - but no per-venue inflight config exists,
and raising it globally would weaken the real-venue brake, so it needs new
plumbing. A decision for broadarrow, not mogwai.
