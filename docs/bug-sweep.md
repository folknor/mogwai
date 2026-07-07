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
a lateral TradeId-length panic found and fixed in convert.rs. See "Found
during fix batches" and "Upstream candidates" at the bottom for new entries.

IDs: E = engine/protocol, D = data path, S = server, AD = adapter data half,
AE = adapter exec half, X = cross-boundary seams. Wave-2 dedupe notes: the
serial havoc-latency throughput cap was reported independently by both adapter
agents (recorded once as AD4); the mirror terminal-state regression by the
exec and seam agents (AE3); the request_bars truncation by the data and seam
agents (AD3); the ModifyRejected venue-id fallback asymmetry by the exec and
seam agents (AE7); the submit-before-dispatch OrderSubmitted by the exec and
data agents (AE8); the min-start_ts cursor drag by the data and exec agents
(AD7).

---

## Engine + protocol (crates/mogwai-engine, crates/mogwai-protocol)

### E3. bug (doc/code contradiction) - known-but-terminal rejects violate the documented `venue_order_id` presence rule

`mogwai-engine` `on_cancel`/`on_modify` vs `mogwai-protocol`
`OrderModifyRejected`/`OrderCancelRejected` field docs and
`reference/architecture.md` ("venue_order_id is absent only when the order id
is unknown"). Severity medium for any consumer keying off presence, confidence
high.

For an id that was accepted but has gone terminal, the engine emits
`venue_order_id: None` with reason "order already terminal (filled or
canceled)" - the id is known (the reason string proves it, via
`seen_client_order_ids`), yet the venue id is absent, because the engine keeps
only a `HashSet` of seen ids and discards the venue id when the order leaves
`open`. An adapter following the documented rule will misclassify
terminal-order rejects as unknown-order rejects. Either the doc rule weakens or
the engine needs an id-to-venue-id map for terminal orders.

Wave-2 resolution: the adapter does NOT rely on wire presence - it keys
known/unknown off its own mirror, so no misbehavior today. But the two reject
paths compensate asymmetrically (see AE7), and the doc/code contradiction
stands.

### E4. gap - Validator disagreement on priceless Market orders; "share the same validators" is not literally true

`mogwai-protocol::validate_submit_order` accepts a `Market` order with
`price: None` (comment and test: Nautilus MARKET orders carry no price). The
engine's `validate_submit` rejects the same order with "submit price
required". A client that pre-validates with the protocol's free validator
passes an order the venue then rejects. Related: the engine never calls
`validate_submit_order`/`validate_modify_order` at all - `validate_submit` and
`on_modify` are parallel re-implementations with different error strings
("quantity must be > 0" vs "submit with non-positive quantity"). Amend-vs-
submit parity inside the engine checks out (qty > 0, qty grid, price > 0,
price grid, notional `checked_mul` all enforced on both paths); the drift is
protocol-vs-engine.

### E5. gap - Stale engine-side divergences are unflushable and the armed queue is unbounded

`mogwai-engine` `arm`/`take_armed`; `control::ClearDivergences` docs. Severity
low-medium (test-harness DoS / cross-test contamination), confidence high;
behavior is partially documented.

`ClearDivergences` deliberately does not flush single-shots, and single-shots
self-disarm on their own trigger - but a targeted `PartialFillNext` whose
order never arrives has no trigger and stays armed forever. No API clears the
engine queue, so control-plane arms accumulate without bound, and a leftover
targeted partial from one test scenario can ambush a later scenario that
reuses the order id.

### E7. gap - A valid partial fraction on a minimum-lot order silently becomes a full fill, and lets a FOK survive

`mogwai-engine`, `fill_quantity`. Severity low, confidence high.

Qty `0.00000001` (one size increment), armed fraction `0.3` (wire-valid):
`candidate = 3e-9` floors to `0`; the `<= ZERO` fallback promotes it to a FULL
fill with only a `tracing::warn`. A FOK order the divergence was armed to kill
therefore fully fills and passes. The grid cannot represent the partial, but a
divergence quietly inverting into its opposite is a surprising failure mode
for a havoc tool, and the warn message ("produced non-positive last_qty")
misleads for a perfectly-validated fraction.

### Engine/protocol smells and nits

- E9. smell - Dead defensive paths: `warn_zero_px` (fill price validated > 0),
  `warn_missing_instrument` and `locked_balances`' missing-instrument
  `continue` are unreachable through `process` (instrument existence is
  validated at submit; no instrument-removal API). The whole `Warned` struct
  services unreachable branches.
- E10. smell - `commission` is always `Decimal::ZERO`; `apply_fill`'s
  commission sign handling (buy adds, sell subtracts - correct) is never
  exercised with a non-zero value anywhere in the crate.
- E11. smell - `seen_client_order_ids` grows forever (documented as
  intentional for terminal-vs-unknown discrimination; also the natural home
  for the venue-id retention fix in E3).
- E15. nit - `validate_submit`'s "market order requires positive price" branch
  is live only as a message differentiator; the generic `price <= ZERO` check
  below covers the same condition.
- E16. nit - protocol's `#[cfg(test)] mod tests` sits mid-file with
  `SubmitOrder`, `ServerMessage`, and the `control` module defined after it -
  legal but disorienting in a 2000-line file.

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

### D4. gap - SessionProfile::validate does not check normalization; a non-normalized profile silently rescales mean duration and vol

`SessionProfile::validate` + `SessionModulator::new`, `generated.rs`. Severity
medium for the config path, confidence high.

The modulator computes `share * 24.0` and `share * 7.0` assuming shares are
fractions summing to ~1 (the fingerprint's do). Validation only requires
strictly-positive-finite. A user config with `intensity_hour = [1.0; 24]` (a
plausible "no modulation" attempt) yields a 24 * 7 = 168x arrival multiplier,
silently compressing the validated `mean_duration_s` from ~7 s to ~43 ms;
`vol_hour` is used raw as a per-mean ratio, so a non-normalized vol curve
silently rescales overall volatility even though `vol_scalar` passed
validation. Either validate sums within tolerance or normalize in
`SessionModulator::new`.

### D5. gap - GeneratorScalars::validate coverage holes vs what the generator assumes

`GeneratorScalars::validate`, `generated.rs`. Severity low-medium each,
confidence high. Fields that pass validation but silently misbehave:

- `modal_tick` vs `price_decimals` consistency unchecked: modal_tick 1e-7
  (in range) with price_decimals 1 (in range) makes `round_dp(1)` collapse
  every price to a 0.1 grid - the configured tick is silently coarsened, and
  the on-grid invariant still holds so tests will not catch it.
- `start_price` unbounded against the hard mid clamps in `next_latent_mid`
  (`.max(tick_f64).min(1_000_000_000.0)`): start_price 5e9 validates, then the
  first tick collapses the mid to 1e9 (an instant 80 percent crash);
  start_price below one tick silently jumps up to the tick.
- `vol_scalar` only checked strictly-positive-finite, but any value above
  ~1e-6 is silently neutered by the sigma2 cap (`(GARCH_SIGMA_CAP *
  clamp_mult)^2`); the knob's documented meaning stops being true above the
  cap.
- `symbol` (serde-defaulted to the empty string) never checked non-empty; a
  config omitting it emits trades with an empty symbol, cross-contaminating
  `TickRuleAggressor` per-symbol state and any symbol-keyed consumer.

### D6. gap - SessionEdgeSpike does not lift the return clamp the way VolStorm does

`RegimeState::new` / `next_latent_mid`, `generated.rs`. Severity low-medium,
medium confidence it is unintentional.

VolStorm sets `clamp_mult = vol_mult` so the lifted RMS is not strangled by
`MAX_ABS_RETURN` (2e-5) - a dedicated test exists to disprove the bare
multiply. SessionEdgeSpike leaves `clamp_mult = 1.0`, so a large
`extra_vol_mult` (validator allows up to 100; combined with the session peak
hour ~2.46 that is ~250x base RMS) clamps a substantial fraction of in-window
draws and the realized spike saturates well below the requested
amplification. The existing test uses 6.0, far below where clamping binds.
Undocumented asymmetry between the two vol regimes.

### Data-path smells and nits

- D7. smell - CheckpointIndex memory grows without bound: one full
  `GeneratedSource` clone (rng, distributions, scalars incl. a heap String)
  pushed every K ticks forever, nothing prunes. Also `max_extend` is per-call:
  repeated far-future polls drag the frontier further per request, so the
  backstop bounds latency per call but not cumulative work. (Same defect
  reported independently by the server agent - see S14; recorded once here,
  server angle there.)
- D8. smell - Panicking public constructors: `GeneratedSource::
  with_clamp_override` (reached from `new` / `new_with_session_profile`)
  expect-panics on invalid scalars or session profile instead of returning
  `Result`, though `ScalarError`/`SessionProfileError` exist. Both inputs
  `Deserialize` straight from user config, so a caller that forgets to
  pre-validate turns a config typo into a process panic.
- D9. smell - lib.rs crate doc drift: describes the whole crate as CSV-backed
  ("Backed by the Kraken trade-history CSV dump...") and never mentions
  `GeneratedSource`, contradicting architecture.md.
- D10. smell - Invalid UTF-8 line kills the rest of a CSV stream:
  `read_line` returns `Err(InvalidData)`, handled as a fatal read error
  truncating the stream - unlike every other malformed-row case, which is
  skipped. Offline lineage only.
- D11. nit - VolStorm's clamp lift touches the feedback path: `base_return`
  (the GARCH feedback) is clamped at the LIFTED cap, and sigma2's cap is
  lifted too, so under a storm the recursion state can differ from clean
  whenever the clean clamps would have bound. architecture.md's "recursions
  consume their un-modulated feedback" is strictly true only for the
  multiplier, not the clamp lift. Deliberate per in-code comments; doc
  tension only.
- D12. nit - `checkpoint_resume_is_byte_identical` compares only (ts, price);
  size and aggressor are not compared. Test name overclaims slightly.
- D13. nit - parse_kraken_ts / parse_kraken_line lenient edges: `"1."`
  (trailing dot) parses as 1 s; a leading `+` on the seconds field is
  accepted; extra columns are silently ignored. Harmless for real dumps.
- D14. nit - First emitted side is always Buyer for every seed
  (`prev_side: Buyer`, low regime, flip prob 0.02): every realization opens
  with a long Buyer run (32 consecutive in the golden stream). A
  seed-independent structural bias at stream start.
- D15. nit - Realized price can drift arbitrarily far from the latent mid:
  `drift_ticks` is an unbounded never-reset random walk; the mid is bounded
  to [tick, 1e9] but the printed price is mid + drift, unbounded above.
  Diffusion is slow; cosmetic, but price is not tethered to `start_price`
  long-run.

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

### S5. bug - Live subscription can silently stream zero frames when the seek exceeds the caps; no error frame, no log

`source.rs` + `main.rs` `spawn_replay`. Severity moderate, confidence high.

Two paths: (a) a regime'd subscribe always takes the fresh from-origin drain;
once `sim_now - data_origin` exceeds `MAX_HISTORY_SEEK_TICKS` (about two wall
hours of uptime at speed 100; identity mode is protected by the default 24 h
horizon), `BoundedSeek::seek_to` returns `None`, MergeSource yields nothing,
and the replay thread logs started/finished back to back while the client sees
a healthy-but-idle feed - exactly the failure mode the ProtocolError work was
done to eliminate. (b) the clean path after long idle: `extend_to` walks at
most 190k ticks per call, then BoundedSeek caps another 190k; a frontier
lagging sim-now by more gives the same silent dead subscribe, and only
repeated resubscribes ratchet the frontier forward. Contradicts
architecture.md's "a fresh subscribe's seek is flat in K no matter how long
the session has run" (true only while the index is kept warm).

### S7. bug (low) - flock/unlink race in `stop` can leave a live daemon unguarded

`main.rs`, `stop`. Severity low (narrow window), confidence high on the
mechanism.

On `LockAttempt::Acquired` (stale file) `stop` unlinks the pid file while
holding the lock. A concurrent `serve` that already opened the same inode but
has not locked yet will lock the now-unlinked inode; its daemon then runs with
its lock on a ghost file, the path is free, and a second `serve` (fresh inode)
can also start - two daemons, and `stop` cannot see the first. Same race class
between `stop`'s unlink and `serve`'s open. The classic
flock-on-unlinkable-pidfile hazard.

### S8. gap - /trades serves the future (look-ahead leak)

`main.rs`, `trades`. Refuses `start < data_origin` but nothing bounds
`start`/`end` at sim-now. A request for a future window extends the shared
index into the future (up to the extension cap) and returns deterministic
FUTURE ticks - a client can read tomorrow's tape. architecture.md says
legitimate windows live in `[data_origin, sim_now]` "by construction", but the
upper bound is unenforced.

### S9. gap - Subscribe with `start_ts < data_origin` silently clamps to the origin

`build_live_source`/`positioned_generator`: a pre-origin target skips the
checkpoint path, the fresh generator starts at origin, the seek trivially
succeeds - the client gets an origin-anchored stream with no indication.
Asymmetric with /trades, which refuses the same window loudly with a 422.

### S10. gap - ProtocolError is execution-category but bypasses the DelayAcks pump

`handle_socket` sends ProtocolError straight into `tx`;
`ServerMessage::category` classifies it Exec. havoc.md says DelayAcks holds
EVERY outbound execution event. Low severity; classifier/route mismatch.

### S11. gap - Writer-side temporal divergences have zero effect under the HttpOrders transport

`submit_order_http` returns engine events synchronously; DelayAcks / GoDark /
StallData live only in the /ws writer. An armed GoDark blacks out a WS client
but an HttpOrders exec client trades straight through. May be intended
(havoc.md frames them as connection-scoped), but no doc states the HTTP order
path is exempt. Low-moderate; needs a doc or design decision.

Wave-2 resolution: CONFIRMED independently at the seam by the cross-boundary
agent (GET /trades polling bypasses the windows too, so under HttpPolling the
whole feed is exempt): a scenario TOML arming GoDark plus `transport_profile =
"HttpOrders"` runs a clean execution path while the operator believes a
blackout is being exercised. Neither agent found the exemption documented.

### S12. gap (doc) - config.md and cli.md omissions

`backfill_horizon_ns` is missing from config.md's key table despite being a
first-class Config field (the data-origin floor). cli.md's `man` topic list
omits `clock` while `ManTopic` in man.rs bundles it - the bundled cli doc
self-describes wrongly.

### Server smells and nits

- S13. smell - One global checkpoint mutex serializes all symbols and holds
  through ~100 ms extensions: `positioned_generator` keeps the
  `checkpoint_store()` guard across `extend_to` (up to 190k synthesized
  ticks). Every live subscribe, seeked /trades, and price-less market order
  for ANY symbol queues behind it. Within stated per-call budget; bursty
  concurrency stacks.
- S14. smell - Checkpoint index grows without bound over uptime: one
  GeneratedSource clone per K ticks per (symbol, data_origin), never pruned;
  a long accelerated run is a slow unbounded memory ratchet (hundreds of
  clones per day per symbol at speed 100). (Same defect as D7, server angle.)
- S15. smell - A panicking replay thread dies silently: `quiesce_replay`
  discards the inner `handle.join()` Result (only the spawn_blocking
  JoinError is logged); an unjoined panic mid-stream just ends the feed -
  client sees an idle-but-healthy socket.
- S16. smell - Order event timestamps are sampled before a potentially
  ~100 ms blocking price synthesis: `process_order_cmd` takes ts at entry,
  then `stamp_market_price` may block on the checkpoint mutex/seek; at speed
  100 the emitted events are stamped ~10 sim-seconds before they logically
  occurred.
- S17. nit - Divergence atomics are all Relaxed; no formal happens-before
  between the /control/divergence 202 response and the writer/pump observing
  the new value. Fine on x86, formally racy for arm-then-immediately-order.
  Reporting agent's confidence that it ever matters in practice: low.
- S18. nit - Re-arming GoDark/StallData replaces the window, so a second POST
  with a smaller ms SHORTENS an in-flight blackout. Store-not-extend
  semantics, undocumented.
- S19. nit - `BoundedSeek::seek_to` drains up to cap+1 ticks (the first
  next_tick is not counted). Harmless off-by-one against a backstop.
- S20. nit - Config lacks `deny_unknown_fields`: a typo'd knob (`gap_cap_m =
  0`) silently falls back to the default, adjacent to config.md's "malformed
  file is a hard error" promise (technically only syntax errors are hard).
- S21. nit - Heartbeat cadence edges: default MissedTickBehavior::Burst means
  a stalled runtime replays missed heartbeats in a burst; `server_heartbeat_ms`
  scaled by a large speed can collapse to the 1 ns floor and become a ~1 kHz
  heartbeat flood per socket. No validation bounds it.
- S22. nit - A subscribe of N symbols spawns N OS threads, client-controlled
  and unbounded (unknown symbols still spawn a thread that exits immediately
  and leaves a dead entry in `replays`).
- S23. nit - `man/render.rs` renders nested lists flat (list_stack depth
  tracked but unused for indent), so architecture.md's nested bullets lose
  hierarchy; loose list items pick up a stray blank line between marker and
  body. Cosmetic.

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

- E3 (venue_order_id presence rule): resolved harmless today - the adapter
  keys off its mirror, not wire presence. Asymmetric compensation recorded as
  AE7; the engine/doc contradiction stands.
- E8 / AE11 (zero-initial backoff spin): confirmed and FIXED in fix batch 1
  at the validator. Residual lifecycle comment cleanup recorded under "Found
  during fix batches" below.
- D1 (exact-ts seek off-by-one): confirmed and FIXED in fix batch 1. The
  adapter-side defense-in-depth gap remains as the downgraded AD8.
- D3 (ReopenGap at_ts at or before the anchor): FIXED in fix batch 1 at the
  generator (consumed at construction with a warning, byte-identical to a
  clean run). Whether the server should ALSO reject it loudly at the API
  boundary is still open as a nice-to-have.
- S2 (resubscribe duplicate window): server half FIXED in fix batch 1
  (resume floor read after quiesce). The adapter-side stale-start_ts
  duplication paths remain - see AD5, AD7 - and the WS drain still has zero
  dedup.
- S5 (silent zero-frame subscription): the adapter has no dead-feed watchdog
  on either transport, so a dead feed is structurally indistinguishable from
  a quiet market - see AD12.
- S11 (HTTP carriers exempt from temporal divergences): CONFIRMED at the seam,
  undocumented.

---

## Adapter, data half (crates/mogwai-adapter: client.rs data side, convert.rs, config.rs, factories.rs)

### AD2. bug - `aggregate_bars` drops the final COMPLETED window of every historical bars request

`client.rs` `aggregate_bars` / `request_bars`. Severity medium-high (warmup
shortfall / always-stale last bar), confidence high.

A bar is only emitted when a later trade crosses `close_ts`; the trailing
`state.active` is never flushed, even when the request's `end` proves the
window fully elapsed. A warmup requesting bars over `[start, end)`
systematically gets the newest bar missing - trades in the last window are
aggregated and discarded. The test
`request_bar_aggregation_closes_on_window_and_drops_partial` pins only the
genuinely-partial case; the complete-but-unflushed case is the same code path
and is wrong. `request_bars` computes `end` but `aggregate_bars` never uses
it.

### AD3. bug - Historical requests silently truncate to one 1000-trade page; request_bars compounds it with a bars-vs-trades unit mismatch

`client.rs` `request_trades`, `request_bars`, `fetch_trades`, `capped_limit`;
seam side: research/broadarrow bridge strategy actor warmup. Severity high
for warmup correctness, confidence high on behavior. (Reported independently
by the data-half and cross-boundary agents.)

Both request handlers issue exactly ONE `GET /trades` page, clamped to
`MAX_HISTORY_LIMIT = 1_000` trades, no pagination loop over `[start, end)`. A
window with more than 1000 trades returns a prefix, and the response still
claims the full start/end - the caller cannot tell it was truncated. For
`request_bars` this compounds: `request.limit` semantically counts BARS to
nautilus (broadarrow's warmup issues `request_bars(..., limit =
warmup.bars)`) but is applied to TRADES here. A warmup asking for N
one-minute bars fetches at most N trades (never more than 1000); on the
fitted tape (~5 trades per bar) that aggregates into roughly N/5 bars, and
because `end = None` with a single page they cover only the OLDEST edge of
the window with a gap up to live. The warmup under-delivers or times out
(`warmup_timeout_live_bars` fatal), and the promised warmup/live splice is
not contiguous. The ceiling belongs per-page with an overlap-and-skip loop -
machinery the adapter already has in `PollCursor`.

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

### AD5. bug - A Subscribe queued across a reconnect is sent twice, triggering the server's resubscribe duplicate-window bug

`client.rs` `subscribe_symbol` + `lifecycle.rs` `run_ws_connection`. Severity
medium, confidence medium-high (narrow window, but the havoc surfaces exist
to create exactly these windows).

`send_ws` pushes into the unbounded `cmd_rx`, drained only while a socket is
up. If a subscribe lands during the backoff window, the reconnect first
replays `on_connect()` = `subscribe_commands(subs)` (the symbol is already in
the table - updated before the send), then the select loop drains the stale
queued Subscribe and sends it AGAIN. The double-Subscribe restarts the
server-side replay, and the queued command can carry a stale (older) start_ts
than the table's advanced cursor, replaying an already-delivered window -
duplicate ticks on the wire, which the WS drain has zero dedup for (mogwai
wire trades carry no id; `emit_trade` forwards everything), duplicating
TradeTicks into nautilus and double-counting bar volume. (The server-side
resume-floor race that originally amplified this - old S2 - was fixed in fix
batch 1; the stale-start_ts duplication path here is adapter-side and still
open.) Fix shape: drain/discard cmd_rx before on_connect, or make on_connect
the only subscribe source after a reconnect.

### AD6. bug - HttpPolling feed wedges permanently and silently after a server restart

`client.rs` `poll_market_data`. Severity medium-high, confidence high.

The poll cursor and the SimClock/data_origin_ns are fetched once at connect.
A restarted server derives a fresh data_origin; the cursor's last_ts (or the
sim-now anchor computed against the stale clock) can precede it, so every
`GET /trades` 422s. The loop does `let Ok(batch) = fetch_trades(...) else {
continue; }` - no log, no error counter, no cursor reset, no clock re-fetch,
no disconnect signal. The feed is dead forever and indistinguishable from a
quiet market (`connected` stays true). `ensure_on_tape` guards only the
request_ handlers, not the poll loop.

### AD7. bug - The earliest-start_ts-on-conflict rule never reaches the server on a live second subscription, and fights the reconnect cursor

`client.rs` `subscribe_symbol`, `advance_sub_start_ts`,
`subscribe_commands`. Severity medium, confidence high on the
never-delivered-backfill half, medium-high on the reconnect-duplicate half.
(The exec-half agent independently flagged the min-drag half.)

On a nonzero refcount, `subscribe_symbol` takes `min(existing, new)` into
`state.start_ts` but sends nothing (Subscribe fires only on 0-to-1). A second
subscriber asking for an earlier start_ts never gets its backfill on the WS
path (on the polling path the existing PollCursor ignores the update
entirely). The stored min then only takes effect at the NEXT reconnect, where
`subscribe_commands` replays `Subscribe { start_ts: min }` - replaying a
range the first subscriber already consumed, i.e. duplicate ticks and
double-counted bars, unless a trade arrived in between and
`advance_sub_start_ts`'s max() pushed the cursor forward again. Min-on-
subscribe and max-on-delivery write the same field with opposing policies;
the result depends on interleaving.

### AD8. gap (downgraded) - The poll cursor has no guard against a server that rewinds: `unseen_from_batch` does not filter `ts_event < last_ts`

`client.rs` `poll_market_data` / `unseen_from_batch`. Severity low (was
medium when the checkpoint off-by-one was live), confidence high on the
missing filter.

The primary trigger - the data-crate checkpoint off-by-one at exact-ts seek
targets (old D1) - was fixed in fix batch 1, so the poll cursor's exact-ts
`start = entry.last_ts` is now served correctly. What remains is the
defense-in-depth gap: if any server bug ever returns a trade with `ts_event <
last_ts` again, `unseen_from_batch` does not filter it (it only skips `ts ==
last_ts` prefixes) and the stale trade is emitted as a duplicate. A one-line
`ts_event < last_ts -> skip` guard would make the cursor robust against
rewinds by construction.

### AD9. bug - Unsubscribe racing an in-flight poll fetch resurrects a stale cursor that a later resubscribe silently resumes from

`client.rs` `poll_market_data` / `unsubscribe_symbol`. Severity low-medium,
confidence high on mechanism; the race window is real but narrow.

The cursor map is locked twice per symbol with an awaited HTTP fetch between.
If the last unsubscribe lands in that window it removes the cursor (and subs
entry), but the post-fetch lock does `entry(symbol).or_insert_with(...)` -
re-creating the entry and advancing it with the fetched batch. The entry then
leaks (poll_symbols no longer lists the symbol), and a later fresh subscribe
finds the stale cursor via or_insert_with and resumes from the OLD position
instead of the new start_ts/sim-now anchor - a surprise history flood or an
ignored requested start. Fix shape: second lock uses get_mut and drops the
batch if the entry is gone.

### AD10. bug - Cross-table bars refcount desync: an unmatched unsubscribe_bars can kill the symbol's live feed for the remaining subscriber

`client.rs` `unsubscribe_bars`. Severity low (requires a misbehaving caller,
but nautilus command replay/races can produce unmatched unsubscribes),
confidence high on mechanism.

The per-BarType refs and the per-symbol SubState.bars are decremented
independently. An unsubscribe_bars for a bar type never subscribed (or a
double-unsubscribe interleaved just right) is a no-op on the bars table but
still calls `unsubscribe_symbol(.., SubKind::Bars)`, decrementing the
symbol's bars count that belongs to a DIFFERENT bar type's subscription. If
that drops total() to 0, a wire Unsubscribe fires and the surviving bar/trade
subscription goes dark. Saturating arithmetic prevents underflow, not
cross-type theft.

### AD11. bug - Week/Month/Year bars pass the time-aggregated gate but are aggregated on wrong windows

`client.rs` `subscribe_bars`/`request_bars` guard + `update_bar_state`.
Severity low (unlikely bar specs for this venue), confidence high.

`is_time_aggregated()` includes Week/Month/Year (research bar.rs), and
`get_bar_interval_ns` returns a 30-day/365-day "proxy for comparing bar
lengths" (nautilus's own comment) - not a calendar interval. The adapter's
`((ts / interval) + 1) * interval` produces epoch-anchored 30-day blocks
instead of calendar months, and epoch-anchored weeks (epoch was a Thursday)
instead of nautilus's Monday-anchored weeks (get_time_bar_start).
Day/hour/minute/second are correctly UTC-aligned.

### AD12. gap - No dead-feed detection anywhere; a zero-frame subscription is structurally invisible

`client.rs`, both transports. Severity medium, confidence high.

Per S5 the server can ack a subscribe and stream zero frames. On the WS path
the default ConnHavoc has idle_timeout_ms = 0 (disabled), and even when
armed, the idle clock resets on ANY application frame (heartbeat, exec
traffic), so a data-silent-but-frame-active socket never trips it -
deliberate for the 4255 reproduction, but it means the CLEAN default has no
watchdog, no counter, no periodic "0 ticks in N s for subscribed symbol" log.
On the polling path errors are fully silent (AD6, AD13). broadarrow cannot
distinguish dead feed from quiet market by design, and there is no
adapter-side diagnostic to consult after the fact.

### AD13. gap - Poll-loop fetch failures are swallowed with no log whatsoever

`client.rs` `poll_market_data`: `let Ok(batch) = fetch_trades(...) else {
continue; }`. Severity low-medium (amplifies AD6), confidence high.

Contrast `request_trades`, which was explicitly fixed to log loudly
("Surface the failure instead of the old silent if-let-Ok drop"). The same
silent-drop anti-pattern the codebase already identified survives here.

### AD14. gap - Missing InstrumentDef on the drain path silently black-holes all data for the symbol

`client.rs` `emit_trade` / `handle_market_message` early-return None from
`instrument_def` with no warning. Severity low-medium, confidence high.

The instruments map is seeded once at connect; a symbol subscribed but absent
from the seed (server config change, later-added instrument) streams into
nothing with zero diagnostics. `ensure_instrument` (which re-seeds on miss)
is used only by request handlers, never by subscribe or the drain.

### AD15. gap - Instruments with currencies unknown to nautilus are dropped silently, cascading into every bar being refused

`convert.rs` `instrument_any` errors on `Currency::from_str` failure
(correct), but `emit_seeded_instruments`, `subscribe_instruments`,
`request_instruments` all swallow it (if-let-Ok / .ok() with no warn).
Severity low-medium, confidence high.

A configured `[[instrument]]` with an exotic base/quote never reaches the
Nautilus cache, so broadarrow's executor refuses every bar for it - the exact
failure mode emit_seeded_instruments's own doc comment exists to prevent -
with no log line pointing at the cause.

### AD16. gap - /clock fetch failure falls back to the identity SimClock while the server may run at speed != 1

`client.rs` `fetch_clock_or_identity`. Severity low, confidence high on
behavior, low on real-world frequency.

One warn line, then every ts_init, havoc sleep, quota interval, backoff and
timeout is scaled on the wrong axis for the life of the connection; nothing
retries the clock fetch later.

### AD17. gap - Spawned request_ tasks outlive stop/disconnect

All request_* handlers spawn onto the global runtime without recording
handles in task_handles; stop() cannot abort them. Severity low, confidence
high.

They keep issuing HTTP requests after disconnect and send into a
possibly-dropped sink (error discarded). Benign in practice (bounded work),
but they also consume HttpQuota budget racing a reconnect. (See also the
exec-side twin for dispatch_order tasks under AE laterals.)

### AD18. gap - architecture.md misdescribes instruments subscriptions

architecture.md says trades/quotes/bars/INSTRUMENTS ride the refcount table.
`subscribe_instruments`/`subscribe_instrument` are one-shot fetch-and-emit;
the unsubscribes are no-ops; no refcount involvement. Fine behavior for a
static venue; the doc is wrong. Doc-only, confidence high.

### AD19. gap - Live bars close only on the next trade; a thin tape delays or forever withholds the closing bar

`client.rs` `update_bar_state` (no timer). Severity low-medium,
design-adjacent, confidence high on behavior.

Under LiquidityDrought / ReopenGap halt, the last completed window's bar is
emitted arbitrarily late (with old ts_event) or never (on unsubscribe/stop
the active bar is discarded). Multiple empty windows emit no bars at all.
Nautilus's internal time aggregator fires on the clock; the adapter
deliberately bypasses it (external bars) but provides no timer replacement.
Arguably realistic venue behavior; broadarrow should know bars can stall on
quiet tape.

### Adapter data-half smells and nits

- AD21. smell - Client havoc corrupts adapter-fabricated bars at trade
  granularity: bars are built AFTER the HavocFilter, so dup/drop of a single
  trade silently reshapes OHLCV instead of duplicating/dropping a bar frame,
  and reordered trades can land in the wrong (later) window since
  update_bar_state folds any ts < close_ts trade into the current window,
  including trades before the window's start. Defensible modeling choice;
  worth a deliberate decision.
- AD22. nit - `date_to_unix_nanos` maps out-of-range datetimes (pre-1970,
  post-2262) to None silently: a far-future start becomes "from origin" and a
  pre-epoch end becomes unbounded rather than an error; combined with
  ensure_on_tape(start=None) passing, a nonsense request degrades quietly.
- AD23. nit - `capped_limit` clamps a 5000-trade request to 1000 with no
  warning and no truncation marker in the response (subsumed by AD3 but
  independently worth a warn).
- AD24. nit - `#[allow(dead_code)]` on both MogwaiDataClient and
  MogwaiExecutionClient struct definitions hides genuinely dead fields from
  clippy forever.
- AD25. nit - `ship_server_havoc` uses `request_timeout_secs(&None, sim)`,
  ignoring the configured conn.request_timeout_secs - inconsistent with
  dispatch_order.
- AD26. nit - Double validation in factories: create runs config.validate()?
  and then Mogwai*Client::new validates again. Harmless, mildly misleading.

### Adapter data-half verified-correct notes

Per the reporting agent: `PollCursor::unseen_from_batch` is correct for the
intended stable-ascending server contract, including cap-mid-timestamp and
empty pages (failure modes are all at the contract edges: 1000+ trades
sharing one ts wedges the cursor forever, inherent to ts-cursors and
undetected; server restart AD6; rewind AD8; duplicates AD5/AD7). Tick and
volume bar specs are refused with ensure! (good); bar ts_event = window
close, correct. Convert precision uses rounding (not truncation) via nautilus
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

### AE1. bug - Synthesized POST-failure rejects pass through the HavocFilter, so drop_prob can permanently wedge an order in Submitted

`client.rs` `dispatch_order` (HTTP error branch). Severity high under armed
drop havoc, zero impact with a clean spec. Confidence high.

On a failed POST for Submit/Modify, `reject_for(...)` is routed through
`dispatch_havoc(&mut filter, ...)` - the same per-dispatch HavocFilter used
for real wire events. A drop_prob draw discards the synthesized
OrderRejected/OrderModifyRejected entirely: nautilus never sees a terminal
event, and the mirror stays Submitted with no venue id (so it is also omitted
from order-status reports). This defeats the documented contract ("a failed
POST synthesizes the matching reject so the order reaches a terminal state
instead of wedging in Submitted"). It is also semantically wrong: the reject
never traveled the wire - it models a purely local transport failure - and
the cancel branch right next to it deliberately bypasses the filter for
exactly that reason (`emit_cancel_rejected`, "the failure is purely local...
nothing for the venue-havoc pipeline to model"). Submit/Modify should get the
same bypass. duplicate_prob similarly double-emits the reject (invalid-
transition log noise); reorder_prob can hold it until the flush.

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

### AE3. bug - Mirror status regresses on reordered/duplicated exec events: no terminal-state guard, no ts ordering guard

`client.rs` `handle_exec_message` (OrderAccepted, OrderCanceled, OrderUpdated
arms), `handle_order_filled`. Severity medium-high under reorder/duplicate
havoc. Confidence high. (Reported independently by the exec and seam agents;
the seam agent notes nautilus's own FSM refuses exactly this regression - no
(Filled, Accepted) arm exists in its transition table.)

OrderAccepted unconditionally sets `record.status = Accepted`; OrderCanceled
sets Canceled; OrderUpdated recomputes from leaves_qty. The engine emits
Accepted and Filled adjacently (immediate fills) - precisely the pair
reorder_prob transposes: the mirror ends Filled then regresses to Accepted
and is stuck non-terminal FOREVER (nothing later corrects it; the order is
terminal at the venue). generate_order_status_reports(open_only) then reports
a phantom open order with full filled_qty - the same phantom-EXTERNAL failure
class the position-drop fix was landed to prevent. Nautilus's own order ends
correctly Filled (Submitted to Filled is legal), so the mirror alone is
corrupted. Impact today is muted only because of X1 (order reports are never
consumed in a real broadarrow run). Cheap fix: never overwrite a terminal
mirror status from Accepted/Updated, and/or ignore events with ts_event <
record.ts_last.

### AE4. bug - handle_account_state applies snapshots in arrival order with no ts_event monotonicity check; reorder/duplicate havoc regresses the position mirror persistently

`client.rs` `handle_account_state`. Severity medium-high under reorder havoc,
benign for exact duplicates (idempotent). Confidence high.

The drop-absent-symbols rule assumes each inbound snapshot is the newest. Two
adjacent AccountStates (two fills in a burst) transposed by reorder_prob
leave the mirror holding the OLDER snapshot: a just-closed position
resurrected (phantom EXTERNAL - exactly the desync the retain exists to kill)
or a just-opened one dropped, and ts_last moves backward (additionally
perturbing in_time_range filtering). Nothing corrects it until the next
fill-driven snapshot, which may be never. Guard: skip any snapshot whose
ts_event is less than the last applied one.

### AE5. gap - Unknown-order OrderAccepted / OrderCanceled / OrderUpdated / OrderFilled are dropped silently, unlike the reject paths which warn

`client.rs` `handle_exec_message`, `handle_order_filled`. Severity low-medium
(observability), confidence high.

The three reject arms log an explicit A.11 warning when the mirror lacks the
order; the Accepted/Canceled/Updated arms return from the else with no log,
and a fill for an unknown order likewise vanishes silently. A fill is the
case where silence hurts most - money moved at the venue and the adapter said
nothing. Matters for any event arriving after reset() cleared the mirror.

### AE6. smell - ExecState grows without bound

`client.rs` ExecState. Severity low, confidence high.

`orders` is never pruned (terminal records and permanently-Submitted strays
from failed WS sends live forever) and `fills` is an append-only Vec. A
long-lived forward run accumulates memory linearly and every report
generation scans it all; every report also re-warns "omitting order status
report" for each venue-id-less stray.

### AE7. nit - OrderModifyRejected does not fall back to the mirror's venue id, unlike the cancel path

`client.rs` ModifyRejected arm vs `emit_cancel_rejected`. Confidence high.
(Reported independently by the exec and seam agents; see also E3.)

`emit_cancel_rejected` does `wire_venue_order_id.or(record.venue_order_id)`
while the ModifyRejected arm emits the wire Option bare, so a known order's
modify-reject reaches nautilus with venue_order_id: None where the equivalent
cancel-reject would carry the id. Harmless to nautilus (optional field,
modify_rejected only restores status) but inconsistent; align when the
engine-side E3 fix lands.

### AE8. smell - submit_order emits OrderSubmitted before conversion/dispatch can fail

`client.rs` `submit_order`. Severity low, confidence high. (Also flagged
laterally by the data-half agent.)

An unsupported side/type/TIF errors out of convert::wire_* AFTER the
Submitted event was queued. The nautilus engine then denies the order
synchronously (Initialized to Denied), and the queued Submitted later hits
the Denied order as an invalid transition (logged error, event dropped). End
state is correctly terminal - no wedge - but it produces a stray event, a
scary log, and on a send_ws failure a permanently-Submitted mirror stray
(feeds AE6). Emitting Submitted after successful conversion+dispatch would be
cleaner.

### AE9. gap - cancel_order/modify_order errors surface nowhere on the WS path once the channel is gone

`client.rs` `dispatch_order` WS branch; research nautilus engine only LOGS an
Err from cancel_order/modify_order (no event). Severity low-medium (requires
exhausted reconnects), confidence high on mechanism.

If run_ws_connection has exited (attempt cap exhausted) or the client is
stopped, a cancel returns Err and the order sits in PendingCancel with no
CancelRejected to restore it - unlike the HTTP profile, which emits
emit_cancel_rejected on transport failure. The WS path could do the same when
send_ws fails.

### AE10. gap - Order-status/position reports filter by ts_last within [start, end], hiding long-quiet open orders

`client.rs` `in_time_range` usage. Severity low-medium depending on how
broadarrow sets bounds; confidence medium.

An open order (or position) with no events since `start` is excluded from
reports even when open_only is requested. If reconciliation passes a
lookback-bounded start, a resting order older than the lookback disappears
from the mass status and may be inferred external/canceled. Real venue
mass-status endpoints return all open orders regardless of last-activity
time. (The seam agent's check found nautilus's manager tolerates this for
open_only=true - it only logs - so no false venue-cancels today.)

### AE12. bug/smell - No backoff at all on the first reconnect after an established connection drops, and the attempt cap can never exhaust under accept-then-die

`lifecycle.rs` `run_ws_connection`. Severity medium, confidence high on
behavior, medium on whether it is intended semantics.

`attempt` resets to 0 on every successful connect_async, and after the inner
loop breaks (Close, error, idle timeout) the outer loop re-dials immediately
- the backoff sleep exists only on the connect_async Err arm. Consequences:
(a) a venue that accepts the WS handshake and immediately closes (or a short
idle_timeout against a stalled server) produces an unthrottled
connect/teardown flood with zero delay between cycles; (b)
reconnect_max_attempts semantically becomes "consecutive failed dials" and
never trips in that scenario. Most production reconnect loops sleep the
backoff after ANY disconnect and only reset the attempt counter after the
connection has stayed up for a grace period.

### AE13. nit - HttpQuota::wait holds the tokio Mutex across the sleep; quota accounting is inconsistent across call sites

`lifecycle.rs`. Severity low, confidence high.

Holding the mutex across the sleep is how spacing is enforced across
concurrent callers (FIFO, arguably by design), but a burst of N dispatches
queues linearly with no cap and no timeout (the request_timeout_secs clock
starts only once the POST is issued), so an order can sit in the quota queue
indefinitely with nautilus seeing only Submitted. Also ship_server_havoc and
the connect-time fetch_clock bypass the quota entirely while
fetch_instruments/fetch_account/fetch_trades/post_order honor it.

### AE14. nit - Repeating timer can fire once past stop_time_ns

`clock.rs` `MogwaiTimer::start`. Severity very low, confidence high.

The loop fires at next_time unconditionally and checks the stop bound only
when scheduling the NEXT fire; a timer with stop < start + interval fires
once past stop. Nautilus's LiveTimer has the same fire-then-check shape, so
this matches the live clock - but nautilus's TestTimer property test asserts
ts_event <= stop_time_ns, so components relying on that invariant could be
surprised.

### AE15. nit - A repeating timer whose start is in the past replays a catch-up burst instead of skipping to now

`clock.rs` `MogwaiTimer::start` vs nautilus LiveTimer (which CAS-adjusts a
past next_time to now with a warning). Confidence high.

With allow_past and an explicit past start_time_ns, MogwaiTimer fires once
per elapsed interval as fast as it can (each with historically-correct sim
ts_event) where LiveTimer would fire once and continue from now.
Deterministic catch-up is arguably better on a sim axis; flagged as a
deviation from the canonical clock. Also set_time_alert_ns for a past alert
fires with ts_event = ts_now + 1ns rather than ts_now due to the
create_valid_interval(0) floor - a 1 ns skew, immaterial.

### AE16. gap - A senderless clock silently drops thread-local timer callbacks; depends on broadarrow's wiring order

`clock.rs` `mogwai_clock_factory` / `MogwaiTimer::start`. Severity
low-medium, confidence medium (depends on construction order not auditable
from this repo alone; the seam agent's check of the kernel ordering suggests
the senders are bound first in the normal path).

The factory closure captures try_get_time_event_sender() at clock CREATION
time. If the node constructs the clock before the runner registers the
time-event sender, the clock is permanently senderless and every RustLocal
callback timer is dropped with only a tracing::warn. The fallback rationale
(Rc cross-thread hazard) is well commented, but a permanently-degraded clock
deserves louder-than-warn handling or a late re-lookup per fire.

### AE17. nit - timer_exists counts expired timers while timer_names/timer_count filter them

`clock.rs`. Exactly mirrors nautilus's LiveClock, so bug-compatible rather
than wrong; the asymmetry is surprising in both places.

### Adapter exec-half laterals

- AE18. nit - handle_account_state drops a balance whose currency string
  fails Currency::from_str silently (bare .ok()?), while every other
  unrepresentable-amount path in the same closure warns.
- AE19. nit - Spawned HTTP dispatch tasks (dispatch_order) are not tracked in
  task_handles, so stop() cannot abort them; a slow POST can emit exec events
  after the client stopped (emitter still holds a live sender clone). (Twin
  of AD17 for the order path.)

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

### X1. gap - Startup reconciliation is a silent no-op for MOGWAI: generate_mass_status never implemented

Adapter: `client.rs` implements the three generate_*_reports but NOT
generate_mass_status. Nautilus: the trait default returns Ok(None)
(research common/src/clients/execution.rs), and the live node's
perform_startup_reconciliation calls ONLY generate_mass_status per client -
on None it logs "No mass status available... (likely adapter error)" and
reconciles NOTHING. Canonical adapters implement it by composing the three
report sets (e.g. the kraken spot adapter). Broadarrow: run-prep enables
reconciliation and continuous position checks but sets
open_check_interval_secs: None - the open-order poll is deliberately off.
Severity high; confidence high on mechanism (traced call-by-call),
medium-high on operational impact (fresh forward runs with no prior position
never notice).

Failure scenario: at every boot, startup reconciliation for MOGWAI warns and
skips. A worker restarted while holding an open mogwai position boots with an
empty cache; the bridge's restart path adopts flat. The venue net is
discovered only by the first periodic position poll (10 s startup delay plus
30 s or more interval), which lands mid-run as an out-of-band venue move /
synthesized reconciliation fill - the late-EXTERNAL-adoption desync the
mirror's phantom-position drop rule was specifically built to avoid.

Corollary: with mass status unimplemented and the open-order poll off,
generate_order_status_reports and generate_fill_reports are NEVER called in a
real broadarrow run (only generate_position_status_reports is, every 30 s or
more). architecture.md's "the three reconciliation report generators rebuild
from that mirror" oversells what is actually consumed.

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

### X5. gap/observation - DuplicateNextFill (and duplicate_prob on fills) can never corrupt downstream state; it only exercises dedup logging

Engine: the duplicate is fill.clone(), same trade_id. Nautilus: the duplicate
is warn-dropped (OrderError::DuplicateFill) BEFORE the
portfolio/position/msgbus, so broadarrow's classify layer never sees it. The
adapter's mirror also dedups (seen_trades). Severity low (design-intent
question); confidence high on mechanics.

A robust OMS should dedup, and it does - but mogwai's docs sell duplicate
fills as an account-corrupting divergence a sandbox cannot produce; on this
stack it is structurally inert beyond a warn line. To drive a broadarrow
verdict the engine would need distinct trade_ids on the duplicate - which
would instead be a genuine double-fill/overfill (note allow_overfills=false
in broadarrow makes THAT path the interesting one).

### X6. smell - HttpOrders profile loses order-command ordering

`client.rs` `dispatch_order`: each Submit/Modify/Cancel spawns a detached
task doing its own POST; nothing sequences them (the HttpQuota mutex is not
acquisition-ordered w.r.t. spawn order). The WS path serializes through one
cmd channel. Severity low-medium; confidence high that the race exists,
medium that broadarrow hits it.

Failure scenario: submit immediately followed by modify/cancel (e.g.
broadarrow's stale-order reconciliation cancel) can arrive at /orders
reversed - a spurious "unknown order" cancel-reject followed by an
accept/fill. architecture.md claims order semantics are "byte-identical
across the two carriers"; the ordering guarantee is not.

### X7. (merged into S11 - temporal divergences do not apply to the HTTP carriers; confirmed and expanded there)

### X8. nit - ProtocolError frames are silently swallowed on the data path

`client.rs` `handle_market_message`: the catch-all arm swallows
ProtocolError; the exec path warns. A version-skewed/malformed Subscribe
rejected by the server is invisible on the data client - the feed just stays
silent until the warmup watchdog fires.

### X9. nit/doc - OMS type: docs say Netting, broadarrow always runs the mogwai exec client at Hedging

architecture.md: "exec building an ExecutionClientCore at OmsType::Netting".
research/broadarrow run-prep venue.rs applied_exec_config unconditionally
sets oms_type = Hedging (both scenario and default paths, pinned by its own
test). The adapter's config default (Netting) is never what runs under
broadarrow. Doc drift only - the config knob exists precisely to allow this.

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

### F1. bug - Wire fill trade ids convert through the panicking TradeId::From on the exec drain

`client.rs` (exec side), the `TradeId::from(fill.trade_id)` in fill handling.
Found by the batch-1 convert agent while fixing the data-side TradeId panic.
Severity medium-high (same feed-killing class as the fixed AD1), confidence
high.

A server-sent (or havoc-corrupted) trade id over 36 chars, empty, or
non-ASCII panics the exec task through nautilus's panicking `From<String>`.
Route through `TradeId::new_checked` with a drop-and-warn like the data side
now does. Belongs to the client.rs batch.

### F2. gap - lifecycle.rs comments and defensive floor are stale against the new conn-havoc validator rule

`lifecycle.rs` `ReconnectPolicy::backoff` doc comment and the
`reconnect_policy_zero_initial_stays_zero` test comment describe the
initial==0-with-max>0 combination the validator now rejects. The test itself
still pins the valid both-zero case and passes. Decide whether the
belt-and-suspenders lifecycle guard wants a nonzero floor for
validation-bypassing callers, and update both comments. Belongs to the
lifecycle.rs batch.

### F3. nit - A market order whose price synthesis fails is rejected with the misleading "submit price required"

`mogwai-server` order path. After the batch-1 stale-price fix, a seek-cap
failure leaves `current_price` returning None and the engine rejects the
price-less order with "submit price required" - wrong story for a client
that correctly sent no price. A dedicated "could not synthesize a market
price" rejection would be honest (adjacent to the S5 silent-empty family).

### F4. gap - Daemon-log integration pin missing

The batch-1 S6 fix logs child startup failures to the configured log, but
nothing integration-tests it. A pin in crates/mogwai-server/tests/daemon.rs
(daemonize with a malformed config, assert the failure lands in mogwai.log)
would keep it honest. Residual: failures BEFORE init_logging in the child
(setsid/redirect errors print to the still-inherited stderr; an init_logging
failure itself is silent post-redirect) remain unlogged - narrow window, no
log exists to write to at that point.

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

---

## Upstream candidates (dependencies we have commit access to)

Findings whose root cause or better fix lives in a dependency. Each needs a
written report (or a direct fix) in the dependency's own repo; the research/
copies are read-only mirrors, the sibling checkouts are the real trees.

### U1. nautilus - LiveTimer fires once past stop_time_ns while TestTimer's property test asserts the opposite invariant

`crates/common/src/timer.rs` (live timer fire-then-check loop) vs the
TestTimer property test asserting `ts_event <= stop_time_ns`. A repeating
timer with `stop < start + interval` fires once beyond its stop bound on the
live clock but never on the test clock - components validated against
TestTimer semantics can be surprised in live runs. mogwai's own clock
mirrors the live behavior deliberately (bug-compatible; AE14).

### U2. nautilus - LiveClock timer_exists counts expired timers while timer_names/timer_count filter them

`crates/common/src/clock.rs`. The three introspection surfaces disagree about
whether a fired-and-done timer still "exists". mogwai mirrors it (AE17).

### U3. nautilus - Startup reconciliation reconciles nothing when generate_mass_status returns None, with only a warning

`crates/live/src/node/mod.rs` `perform_startup_reconciliation`. When an
adapter does not implement `generate_mass_status`, the node logs "likely
adapter error" and skips reconciliation entirely instead of falling back to
the three per-kind report generators the adapter DOES implement. Any adapter
that implements the granular generators but not the composite silently gets
zero startup reconciliation (mogwai's X1 is exactly this; the mogwai-side fix
is to implement the composite, but the silent-skip-with-fallback-available is
an upstream design gap).

### U4. broadarrow - The open-order poll is disabled, leaving order/fill report generators dead code

`crates/run-prep/src/lib.rs` sets `open_check_interval_secs: None`, so
`generate_order_status_reports` / `generate_fill_reports` are never called in
a real run (only the position poll runs). Either enable the open-order poll
for the MOGWAI venue or document that order-level reconciliation is
deliberately position-only. Interacts with X1: with mass status implemented
mogwai-side, startup reconciliation starts working, but continuous
order-level healing stays off.

### U5. broadarrow - Inflight escalation constants make any ack delay past ~30 s a guaranteed phantom local reject against mogwai

run-prep arms `inflight_check_threshold_ms` 5 s / `inflight_max_retries` 5
while mogwai's DelayAcks validates up to one hour and client latency up to
60 s (X3). Not a bug on either side alone, but the pairing means a whole
band of mogwai's advertised havoc cannot be exercised without tripping
INFLIGHT_TIMEOUT first. Worth a deliberate broadarrow-side decision (raise
the ceiling for the MOGWAI venue, or document the ~30 s cliff as the
intended brake trigger).
