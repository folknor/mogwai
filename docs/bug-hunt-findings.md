# Project-Wide Bug Hunt and Duplication Findings

Consolidated findings from a multi-agent sweep of the mogwai workspace. Every
finding the agents surfaced is recorded here verbatim-in-substance, nothing
capped or dropped. Labels: **bug** (real defect), **gap** (missing guard /
incomplete contract), **smell** (fragile or confusing but not yet wrong), **nit**
(minor).

Status column is for triage during follow-up: `open` until someone confirms or
fixes. Line numbers drift; treat them as a starting point, not gospel.

Nine agents in three waves: Wave 1 + Wave 2 hunted bugs across all five crates and
the tests; Wave 3 hunted duplicated logic from three lenses. Findings cross-link
(e.g. the missing `validate_divergence` is both a bug enabler and a consolidation),
and those links are called out inline.

---

## Most serious, across all waves (triage shortlist)

These are the findings most likely to cause a real failure on the live-path / havoc
surfaces this system exists to exercise:

- **Duplicate-fill double-counts the adapter mirror** (A.1) - inflates `filled_qty`
  past `quantity`, corrupts `avg_px`; no `trade_id` dedup. Fires precisely under the
  `DuplicateNextFill` divergence.
- **WS reconnect replays the full market-data history** (A.2) - no cursor on the WS
  path, so connection-lifecycle havoc deterministically floods duplicate ticks after
  the first reconnect.
- **Inbound Decimal->f64 `expect` panics on the hot path** (A.8 / D.1, root cause in
  dup #2) - a pathological wire/control-plane Decimal crashes the adapter instead of
  degrading; the data crate already saturates.
- **`PartialFillNext` head-of-line-blocks the armed queue + zero/negative fraction**
  (B.1, B.2, dup #16) - a mistargeted divergence silently disarms everything behind
  it; an unvalidated `fraction` emits zero-qty or negative fills. No
  `validate_divergence` exists.
- **Market-order fills price at zero** (B.3) - market buys credit base for free and
  debit zero quote, corrupting balances.
- **`BoundedSeek` returns pre-`start` ticks at the cap** (E.3) - breaks the `/trades`
  cursor contract the adapter's `PollCursor` relies on.
- **Replay pacing truncates sub-ms gaps to zero** (E.4) and **overlap/resubscribe
  double-feeds** (E.5, E.6) - break per-symbol ascending-`ts_event` ordering.
- **`GoDark` is process-global and drops (not delays) frames** (E.1, E.2) - blacks out
  all clients and loses execution events permanently, with no clear path (E.15).
- **`connect()` timeout leaks the reader task and double-spawns on retry** (A.3).
- **Clock-skew panics** in both `now_unix_nanos`/`now_ns` (A.6 / E.10, fix once via
  dup #1).
- **Test suite proves less than it claims** (F.1-F.6, F.13) - variant/count-only
  assertions and wrong-sided tolerances would pass a mis-mapped fill; partial-fill,
  duplicate-fill, dropped-account-update and blackout behavior are untested.

---

## Wave 1 - bug hunt, the heavy cores

### A. `crates/mogwai-adapter/src/client.rs` (~3269 lines)

1. **bug - Duplicate fill double-counts the order/position mirror**
   (`handle_order_filled` ~2156-2247; dup logic `HavocFilter::emit_candidates`
   ~1298-1314). When `duplicate_prob` fires or the server sends `DuplicateNextFill`
   (two identical `OrderFilled` frames), `handle_order_filled` runs twice for the
   same trade: each pass does `record.filled_qty += fill.last_qty`, recomputes
   `avg_px`, flips status, and pushes a second `FillRecord`. A single economic fill
   inflates `filled_qty` (can exceed `quantity`), corrupts `avg_px`, and yields two
   fill reports for one `trade_id`. No `trade_id` dedup anywhere. The reconciliation
   mirror - the whole point - becomes wrong precisely under the duplicate-fill
   divergence the system exists to inject.

2. **bug - WS reconnect replays the full market-data history (duplicate ticks)**
   (`subscribe_commands` ~809-828; `connect` ~315; `SubState.start_ts` ~117-121).
   On the WS data path `start_ts` is pinned to the original subscription instant and
   never advanced. On every reconnect, `on_connect` re-issues `Subscribe { start_ts }`
   from that original instant, so the server replays the entire history again. Unlike
   the polling path (which has `PollCursor` dedup), the WS path has no cursor, so the
   connection-lifecycle havoc surface (idle timeout, reconnect) deterministically
   floods the strategy with duplicate trades/bars after the first reconnect.

3. **bug - `connect()` timeout leaks the reader task and double-spawns on retry**
   (`MogwaiDataClient::connect` ~305-343; `MogwaiExecutionClient::connect` ~1630-1656).
   `wait_connected` returns `Err` after 5s, but the spawned `run_ws_connection`
   reader task is already in `task_handles` and keeps looping/reconnecting. The
   `connect` error propagates but nothing aborts that task. A retry spawns a second
   reader, overwrites `self.ws_cmd`, and orphans the first (still flipping the shared
   `connected` flag). Task/socket leak plus a stale task racing on the `connected`
   AtomicBool after a slow connect.

4. **bug - `OrderUpdated` ignores `leaves_qty`, leaving mirror `filled_qty` stale
   after an amend** (`handle_exec_message` OrderUpdated arm ~2077-2130). Wire
   `OrderUpdated` carries `leaves_qty` (new remaining after amend) but the handler
   destructures with `..` and never reads it. It overwrites `record.quantity` but
   leaves `filled_qty` untouched, so after a downsizing amend on a partially filled
   order, `quantity - filled_qty` no longer equals `leaves_qty` and status logic can
   be inconsistent with the venue. The comment at ~2098-2100 claims a correctness it
   doesn't enforce.

5. **bug - Reorder transposition can starve indefinitely / mis-pairs across unrelated
   streams** (`HavocFilter::apply` ~1277-1289). When `reorder_prob` fires the current
   message is stashed in `self.held` and nothing is emitted; released only on the next
   message or `flush()` at disconnect. On a low-traffic exec stream a held
   `OrderAccepted` can sit unemitted arbitrarily long. On the data WS path the same
   filter handles trades and quotes interleaved, so the held message may transpose past
   an unrelated symbol/type. In the HTTP-orders path each dispatch builds a fresh
   `HavocFilter` then immediately `flush`es (~1508-1519), so reorder is a no-op there
   while drop/duplicate still apply - transport-dependent havoc semantics.

6. **bug - `now_unix_nanos` panics in ~2554 and casts `u128 as u64`** (~1401-1408).
   `duration_since(UNIX_EPOCH).expect("clock before epoch")` panics on any backward
   clock step (NTP/leap). `.as_nanos() as u64` truncates silently past year 2554. The
   reader/dispatch task it runs in is `tokio::spawn`ed with no supervisor, so a
   transient skew silently kills the data/exec stream.

7. **bug - Error-swallowing on the execution-critical path when instrument def is
   missing** (`handle_account_state` / `handle_order_filled` ~2158-2160, ~2267-2269).
   Both silently `return` if the instrument def is missing, dropping the event with no
   log. Under `DropNextAccountUpdate` this is intended, but a legitimate missing def
   (instrument cache not yet seeded) silently swallows a real fill/account update and
   leaves the order in `Submitted`/`Accepted` forever.

8. **smell - `to_f64().expect("decimal fits f64")` can panic on adversarial decimals**
   (convert.rs 18, 22; client.rs 1786, 2217, 2257-2259). `Decimal::to_f64()` returns
   `Option`; these `expect` it. The system's purpose is injecting hostile values; a
   crafted commission/balance/price with extreme scale could yield `None` and panic the
   exec task.

9. **smell - Free functions `.expect("... poisoned")` while instance methods return
   `Err`** (815, 951, 966, 1051, 1078, 1112, 2198, 2265, 2293, 2305). The `&mut self`
   subscription methods propagate mutex-poison as `anyhow::Err`, but the free functions
   invoked from spawned tasks `.expect()` and panic. One panic while holding
   `subs`/`state` cascades into killing the reader/poll tasks.

10. **gap - `request_quotes` always returns an empty quote response** (~500-517).
    Synthesizes `QuotesResponse` with `Vec::new()` unconditionally, never hits the
    server. A strategy requesting historical quotes silently gets nothing - not an
    error, not data - with no comment marking it deliberate (unlike other mappings).

11. **gap - `OrderRejected` / `OrderModifyRejected` for unknown orders silently
    dropped** (~2031-2038, ~2137-2140). If the venue rejects an order the mirror
    doesn't know (`order_record` returns `None`), the handler `return`s and no
    rejection reaches nautilus. For `OrderModifyRejected` with `venue_order_id: None`
    (protocol's explicit "id unknown to venue" case, ~740-742) this is exactly when
    you'd want to surface it. A strategy that submitted via a path the mirror missed
    (or after a reset cleared `orders`) never learns its order was rejected.

12. **gap - Exec client has no `reset`; `ExecState` is never cleared**.
    `MogwaiDataClient::reset` (~207-222) clears its maps, but `MogwaiExecutionClient`
    has no equivalent, so `ExecState.orders/fills/positions` survive a stop/start.
    Combined with reconnect re-subscribe, reports can include orders from a prior
    session.

13. **smell - `client_havoc_for_dispatch` perturbs the seed only when `seed.is_some()`**
    (~1345-1351), and the fresh-filter-then-flush HTTP pattern (~1508-1519) makes
    reorder a no-op over HTTP while drop/dup still apply - havoc not equivalent across
    transports despite shared config.

14. **nit - `event_kind` maps `AccountState` to `EventKind::Data`** (~1356-1358). Per
    spec (`HavocLatency.data_nanos` covers account-state snapshots) this matches, but
    it's surprising that an exec-stream message is delayed by the data latency knob
    rather than `exec_event_nanos`. Verify intent.

15. **nit - `PollCursor::unseen_from_batch` assumes server returns trades sorted/grouped
    by `ts_event`** (~693-716). Dedup skips N already-emitted trades at the front of the
    next batch; only correct if the server returns ascending `ts_event` with stable
    same-timestamp ordering. Undocumented coupling to server ordering.

16. **nit - `capped_limit` clamps to 10_000 even on history requests asking for more**
    (~1377-1381). Silent truncation with no "truncated" flag in the response; bars
    aggregated from a truncated trade set miss the tail of the window.

17. **nit - `wait_connected` polls a `connected` flag the reader flips per-reconnect**
    (~1410-1419; lifecycle 147/154/163/254). Returns `Ok` the instant it first sees
    `true`; a flap can flip it back to `false` right after, so `connect()` returns
    success while the stream is actually down, and `is_connected()` then reports
    `false`. Racy against the lifecycle havoc that closes sockets quickly.

> Most serious in A: #1 duplicate-fill mirror double-count, #2 WS-reconnect history
> replay, #3 connect-timeout task leak - all sit squarely on the havoc surfaces this
> adapter exists to exercise.

### B. `crates/mogwai-engine/src/lib.rs` (~1146) + `crates/mogwai-protocol/src/lib.rs` (~837)

1. **bug - Mistargeted `PartialFillNext` head-of-line-blocks the entire armed queue**
   (engine ~188-198). The `PartialFillNext` arm fires only when `*client_order_id ==
   order.client_order_id`. If a front-armed divergence targets "O2" but "O1" is
   submitted first, the non-matching divergence stays at `front()` and is never popped,
   blocking every subsequent engine-side divergence (DuplicateNextFill,
   DropNextAccountUpdate) forever. The `arm()` comment (~131-134) already worries about
   stale temporal settings blocking front() but does not guard this data path.

2. **bug - Zero-fraction partial fill emits a spurious `OrderFilled` with
   `last_qty == 0`** (engine ~200-225). `PartialFillNext { fraction: 0 }` is a valid
   wire value (no `validate_divergence` exists). With `fraction = 0`, `last_qty = 0`,
   and the engine still pushes `OrderFilled` with a `trade_id`. A real venue never
   reports a zero-qty fill; the adapter/broadarrow may double-count or choke. Every
   accepted order emits a fill event even when nothing filled.

3. **bug - Market-order fill prices at zero and books a zero-notional fill** (engine
   ~200-202, ~374-382). For a market order (`price == None`), `last_px =
   Decimal::ZERO`. `notional = last_qty * 0 = 0`, so a market buy credits base quantity
   but debits zero quote - the account acquires BTC for free. `warn_zero_px` fires but
   the balance math is still wrong.

4. **bug/gap - Engine `arm()` silently swallows `DelayAcks`/`GoDark`** (engine ~135).
   These are server-owned per the comment, but `HavocSpec.server: Vec<Divergence>`
   (protocol ~52) carries them (test at protocol ~524 puts `GoDark` in `server`). If
   the server forwards the whole `server` vec to `engine.arm()`, these two of six
   variants are silently discarded. Coordination hazard - verify the server's outbound
   layer handles them and never relies on `arm()` to enqueue.

5. **bug - `SessionEdgeSpike` validation allows `end_hour == 24` with no `start_hour <
   24` check** (protocol ~175). Check is `start_hour >= end_hour || end_hour > 24`. A
   consumer indexing a 24-element session curve by `end_hour` could go out of bounds at
   24. Latent off-by-one trap for the data crate (bound semantics live in the consumer).

6. **gap - `ReopenGap` validation never validates `at_ts`** (protocol ~184-197). Every
   sibling regime constrains its numeric fields; `ReopenGap` destructures `at_ts` away
   with `..`. `at_ts = 0` (test ~415) means halt at epoch - immediate/never for a
   forward replay. Unvalidated knob.

7. **smell - `validate_conn_havoc` accepts `reconnect_delay_max_ms = 0` with nonzero
   initial** (protocol ~108-122). The max-vs-initial check is guarded by both being
   `> 0`, so `initial = 5000, max = 0` passes, yet `max = 0` is documented nowhere as
   "unlimited" (only `reconnect_max_attempts: None` / `max_requests_per_second: None`
   are). Undefined meaning silently accepted.

8. **smell - `RejectNextSubmit` ignores `client_order_id` while `PartialFillNext` honors
   it** (engine ~166-178; protocol ~826-827). `RejectNextSubmit` has no
   `client_order_id` field, so it rejects whatever order comes next regardless of intent
   - inconsistent targeting model across divergences.

9. **smell - Mixed additive/multiplicative vol composition is a latent trap** - see also
   data finding C.5; surfaced here as the protocol/engine contract that permits both a
   `vol_mult` and an edge spike to be set.

10. **smell - `VolStorm` validation is redundant** (protocol ~156-162). Range
    `(0.0..=100.0)` already includes 0.0, then `vol_mult > 0.0` excludes it again. Net
    `(0.0, 100.0]` is correct but the logic is redundant/confusing.

11. **smell - `next_id`/`seq: u64` increments without overflow guard, shared across
    `V-`/`T-` prefixes** (engine ~81, ~140-144). Globally monotonic; overflow at
    `u64::MAX` would panic in debug / wrap in release but is unreachable in practice.

12. **smell - Modify rejects `new_total == filled` exactly** (engine ~298-336,
    specifically ~310) as "below already-filled" even though shrinking-to-exactly-filled
    (cancel remainder) is arguably valid.

13. **nit - `HavocSpec.data` uses `skip_serializing_if` but `conn` does not** (protocol
    ~55). `data: None` disappears from JSON while `conn: default` stays - asymmetric
    serialization policy (intentional per comment).

> Most load-bearing in B: #1 mistargeted `PartialFillNext` disarms the queue, #3
> zero-price market fills corrupt quote balances, #2 zero-fraction spurious fills - plus
> the #4 cross-crate `DelayAcks`/`GoDark` swallow to verify against the server.

### C. `crates/mogwai-data/src/generated.rs` (~1371) + `src/lib.rs` (~495) + `examples/peek.rs`

1. **bug/gap - `validate` never checks `vol_hour` positivity** (generated ~243-264;
   `Fingerprint::from_repo_json` asserts positivity only for `intensity_hour` and
   `dow_weight`, ~72-88). A zero or negative `vol_hour[h]` silently zeros or negates
   realized returns for that hour (line ~392 multiplies the return by `vol_mult`); a
   negative value inverts price direction with no guard. Incomplete fail-loud contract.

2. **bug/semantic - Lognormal `typical_size` is the median, not the mean** (generated
   ~316-317). `LogNormal::new(size_median.ln(), SIZE_LOG_SIGMA)` makes `typical_size`
   (0.1) the median; the actual mean is `exp(mu + sigma^2/2) = 0.1 * exp(1.15^2/2) ~=
   0.194`. If any consumer expects mean trade size to equal `typical_size`, it's ~2x too
   large. The realism test only checks `size_cv > 0.5`, so this is unguarded.

3. **smell - Hardcoded literals duplicated across `from_fingerprint_medians` and
   `xbtusd_anchor`** (generated ~223-224, ~234-239). Both hardcode `start_price =
   60_000`, `typical_size = 0.1`, `vol_scalar = 5e-8`, none validated against any
   fingerprint range. `from_fingerprint_medians` pairs a 60000 BTC-scale price with
   `modal_tick = 0.0001` / 4 decimals - an unrealistically fine grid for that price
   level. A shared default would prevent drift.

4. **gap - Fine-grid (multi-decimal tick) price path is untested** (generated ~403). The
   realism test only runs `xbtusd_anchor` (tick 0.1, 1 decimal); the on-grid invariant
   `(price/modal_tick).fract() == 0` is only asserted for the anchor. The
   `from_fingerprint_medians` tick-0.0001 path that accumulates f64 error before
   `round_dp(4)` is never exercised.

5. **smell - Mixed additive/multiplicative vol composition** (generated ~392, ~527-537).
   `next_latent_mid` does `session.vol_mult * regime.vol_mult`, where `regime.vol_mult`
   returns `self.vol_mult + edge_mult` (additive). A `VolStorm` multiplies; a
   `SessionEdgeSpike` adds (factor `1.0 + extra`). No current path sets both (the match
   is exclusive), but a future regime setting both would have the edge add to the storm
   baseline instead of multiplying - correctness trap.

6. **gap - `take_reopen_crossed` measures the halt from the crossing trade, not from
   `at_ts`** (generated ~539-549, ~435-437). The clock advances by `dt_ns` past `at_ts`
   first, then adds `halt_ns`, so the realized halt window can be up to one
   inter-arrival longer than `halt_secs`. The test tolerates this (`dt >= halt_ns`).
   Minor semantic imprecision.

7. **nit - EOF and IO error collapsed in `KrakenCsvSource::next_tick`** (lib ~202).
   `Ok(0) | Err(_) => return None` swallows a mid-file read error as a clean
   end-of-stream, truncating data with no diagnostic.

8. **smell - Hand-rolled Lanczos `gamma` used once at construction** (generated
   ~683-710). Computes `weibull_mean(0.60)` once; `rand_distr::Weibull` is already a dep
   and the mean is `scale * gamma(1 + 1/k)`. ~25 lines of recursive special-function
   code nothing else calls; the reflection branch (`z < 0.5`) is dead for the only
   caller (`z = 2.667`).

9. **nit - `next_price` has an `unreachable!` on `AggressorSide::NoAggressor`**
   (generated ~407). Genuinely unreachable by construction (`next_side` never returns
   it), but it's a panic path depending on an invariant two functions away.

10. **nit - `session_modulation_reproduces_curves` is `#[ignore]`d and depends on the
    centering convention** (generated ~1032). Asserts raw fingerprint shares against
    measured occupancy fractions; works only because `SessionModulator` centers the
    multiplier on 1.0. Changing the centering breaks this in a non-obvious way and CI
    won't catch it (ignored).

> Most worth fixing in C: #1 unguarded `vol_hour` (silent flip/zero of an hour's
> returns) and #2 the lognormal median-vs-mean semantic.

---

## Wave 2 - bug hunt, the rest

### D. adapter `config.rs` / `convert.rs` / `factories.rs` / `lib.rs` / `lifecycle.rs`

1. **bug - `convert.rs` `price`/`quantity` panic on non-finite or out-of-range
   decimals** (`convert.rs` ~17-23). `Price::new(d.to_f64().expect(...), precision)` and
   the `quantity` twin call nautilus `Price::new`/`Quantity::new`, which `assert!`
   (panic) when the f64 is NaN/inf or outside `[PRICE_MIN, PRICE_MAX]` /
   `[0, QUANTITY_MAX]`. Inputs are `Decimal`s straight off the wire that the server can
   emit at hostile magnitudes under market-regime havoc. `Quantity::new` rejects
   negatives, so any negative `size`/`leaves_qty`/`bid_sz` panics the whole adapter
   task. The point of mogwai is to feed ugly data; a panic here downs the data/exec
   client instead of surfacing a degenerate tick. Use `new_checked` and drop/log.

2. **bug - Untrusted `price_precision`/`size_precision` can panic via
   `check_fixed_precision`** (`convert.rs` ~17-23). Precision flows from
   `InstrumentDef.price_precision`/`size_precision` (`u8`, 0..=255). `Price::new`
   panics when precision exceeds nautilus `FIXED_PRECISION` (9, or 16 in
   high-precision builds). A server/havoc'd instrument advertising `price_precision: 50`
   panics on the first tick. Same `new_checked` remedy.

3. **bug - `lifecycle.rs` backoff ignores the ceiling when
   `reconnect_delay_max_ms == 0`** (`lifecycle.rs` ~44-58). `backoff` returns base `0.0`
   when `initial_ms == 0.0 || max_ms == 0.0`. With `initial > 0` but `max == 0`, a user
   reasonably expects "no ceiling," but the code collapses the delay to zero - a tight,
   CPU-spinning reconnect loop. `validate_conn_havoc` permits the combination. Treat
   `max == 0` as "no clamp" or reject it.

4. **gap - `config::validate` never checks `base_url` is a parseable ws/wss URL**
   (`config.rs` ~48-58, ~111-121). Only rejects empty/whitespace. `"http://x"` or
   `"not a url"` passes here, then `connect_async` fails at connect time and the failure
   is swallowed into the reconnect loop (`lifecycle.rs` ~153), so a typo'd URL manifests
   as a silent never-connecting client. `http_base_url` also passes through anything not
   prefixed `ws://`/`wss://` unchanged. Validate scheme/parse up front.

5. **smell - `lifecycle.rs` silently drops unparseable server frames** (~214, ~220).
   `if let Ok(server_msg) = serde_json::from_str/slice(...)` swallows deserialization
   errors with no log. A protocol-version skew or malformed `ServerMessage` is invisible;
   the idle clock is already reset so the connection looks healthy while data is dropped.
   Log the parse error.

6. **smell - Reconnect-jitter RNG seeding vs documented determinism** (`lifecycle.rs`
   ~142). Unseeded path uses `from_entropy` (expected), but confirm the caller (client.rs)
   passes the havoc `seed` so reconnect jitter is reproducible when a seed is configured;
   if only the inbound-corruption RNG is seeded, reconnect jitter is non-reproducible
   despite a configured seed. Cross-check with client.rs.

7. **smell - `factories.rs` hard-codes `OmsType::Netting`** (~102-110). No config knob for
   OMS type while `account_type` is configurable. Confirm broadarrow doesn't expect
   Hedging for MOGWAI; a wrong OMS silently changes position accounting.

8. **smell - `lifecycle.rs` heartbeat `interval` fires its first tick immediately**
   (~189-194, ~199-204). A Ping is sent right after connect rather than after
   `heartbeat_interval_ms`; cadence is off by one interval on every (re)connect.

9. **smell - `lifecycle.rs` quota interval rounds down** (~74-79). `min_interval =
   1e9 / max` integer division floors, so a non-evenly-dividing `max` (e.g. 3 r/s) yields
   spacing a hair short and an effective rate marginally above the cap.

10. **smell - `convert.rs` `instrument_id` takes `&InstrumentDef` but only reads
    `def.symbol`** (~33-35), and always uses `*MOGWAI_VENUE`. The broad parameter invites
    a caller to assume per-def venue handling that doesn't exist.

11. **nit - `convert.rs` `TradeId` from `symbol-ts_event` is not collision-free** (~48).
    Two trades for one symbol sharing a `ts_event` (same ns, plausible under bursty
    synthesis) collide; the wire `TradeTick` carries no native id. Duplicate `TradeId`s
    can confuse nautilus dedup/caching - notable given the project deliberately emits
    duplicates.

12. **nit - `lifecycle.rs` `writer_handle`/`reader_handle` aborted but never awaited**
    (~251-253). Abort is best-effort; in-flight writer sends may still race. Likely
    benign during teardown but the ordering assumes prompt abort observation.

13. **nit - `config.rs` `http_base_url` leaves a bare/relative `base_url` untouched**
    (~138-146). A value with no recognized scheme yields an HTTP base that isn't HTTP.
    Tied to #4.

14. **nit - `factories.rs` duplicates the `"MOGWAI"` literal** instead of sourcing from
    `MOGWAI_VENUE`; a future venue rename won't propagate. Cosmetic.

> Cross-cutting: the config path is rigorously range-checked, but the runtime data path
> (convert.rs) is the unguarded panic surface - that asymmetry is the biggest real risk
> here. Strongest items: #1, #2 (convert panics) and #3 (max_ms==0 tight loop).

### E. `crates/mogwai-server/src/main.rs` + `src/source.rs`

> **Cross-crate verdict on B.4 (`DelayAcks`/`GoDark`):** the server handles these itself.
> `arm_divergence` (main.rs ~149-158) matches `DelayAcks`/`GoDark` before the catch-all,
> storing them into `state.delay_ms`/`state.dark_until_ns`, and only the `engine_div` arm
> calls `engine.arm()`. The engine's silent-drop of those two variants is never exercised
> by this server. So B.4 is not an active bug - but see E.smell on the latent coupling.

1. **bug - `dark_until_ns` is process-global** (main.rs ~96, ~153-156, ~282, ~292).
   `GoDark` stores an absolute deadline into shared `AppState`, so every writer on every
   connection consults the same atomic - arming `GoDark` darkens ALL live clients, not a
   targeted session, with no way to scope it. Same for `DelayAcks` via `delay_ms`. For a
   harness arming divergences against a specific session this hits unrelated connections.

2. **bug - `GoDark` drops frames instead of holding them** (main.rs ~292-294). During the
   dark window the writer does `continue`, permanently discarding the message - including
   `OrderAccepted`/`OrderFilled`/`AccountState` execution events. A real venue blackout
   delays/queues; here events are gone forever, leaving a hole in the execution stream
   (e.g. a fill never seen) that can desync the adapter's account state.

3. **bug - `BoundedSeek::seek_to` can return a tick before the requested start when the
   seek cap is hit** (source.rs ~104-112). The loop advances while `ts < start_ts &&
   drained < cap`; if `drained` reaches `cap` (50,000) first it returns a tick with
   `ts < start_ts`. `bounded_trades` then emits trades before the requested `start`,
   violating the cursor contract `/trades` pagination relies on (test
   `generated_history_is_replayable_and_cursorable` asserts `second.first().ts_event ==
   cursor`). Also `drained` doesn't count the initial `next_tick`, so the cap is off by
   one - minor vs the early-return defect.

4. **bug - Replay pacing truncates sub-millisecond gaps to zero** (main.rs ~394-401).
   `wait_ms = (gap_ns as f64 / speed) as u64 / 1_000_000` integer-divides ns to ms, so
   any inter-tick gap under `speed` ms becomes `0` and is sent with no delay. Sub-ms
   trade bursts (the generator can emit ticks microseconds apart) aren't paced at all
   while only large gaps are throttled - the burst/lull shape `speed` should scale is not
   preserved.

5. **bug - Overlapping (non-identical) subscriptions double-feed shared symbols**
   (main.rs ~321-326, ~354-359). `sub_key` sorts+dedups the symbol vec so an identical
   resubscribe cancels the prior replay, but `[A,B]` then `[B,C]` produces two keys and
   two live replays both emitting `B` from independent generators with independent
   seeds/clocks. The client gets duplicated, interleaved, out-of-order-per-symbol `B`
   trades, breaking the ascending-ts ordering `PollCursor` depends on. Nothing prevents
   or detects it.

6. **bug - Resubscribe race interleaves stale data** (main.rs ~321-326 + `spawn_replay`).
   On resubscribe the old `cancel` flag is set, but the old replay thread checks `cancel`
   only at the top of its loop and may be blocked in `tx.blocking_send` or mid-
   `next_tick`. It can deliver one more tick into the shared `tx` after the new replay
   begins, producing an out-of-order/duplicate tick at the seam. No handshake/generation
   counter guarantees the old thread is quiesced first.

7. **smell - Detached replay threads are never joined and can linger under churn**
   (main.rs ~345-351, ~381, ~413). On disconnect the handler signals `cancel` and drops
   `tx`, but a thread parked in `next_tick()` (pure CPU generation; with `speed == 0.0`
   default there's no sleep) only notices cancellation at the next loop top. Nothing
   tracks or bounds how many such threads accumulate under rapid connect/subscribe/
   disconnect churn.

8. **smell - `is_execution_event` defined by exclusion** (main.rs ~361-363). Returns
   `true` for anything not `Trade`/`Quote`. A future market-data variant (`Snapshot`,
   `BookDelta`, `Heartbeat`) would be silently treated as an execution event and delayed
   by `DelayAcks`. Prefer an allow-list / a method on `ServerMessage`.

9. **smell - `serde_json::to_string(&msg).expect(...)` in the hot writer path** (main.rs
   ~295). A single un-serializable value would panic the writer task for that connection
   (in a spawned task, silently). Log + skip is more robust for an unattended server.

10. **smell - `now_ns()` truncates `u128` to `u64` and `expect`s on clock skew** (main.rs
    ~100-105). The narrowing cast is practically harmless until ~2554, but
    `duration_since(UNIX_EPOCH).expect(...)` panics every order path / divergence arm on a
    backward clock step. Use a saturating fallback. (Mirrors client.rs A.6.)

11. **smell - Latent coupling on `DelayAcks`/`GoDark` server-ownership** - the server
    correctly handles them today, but the split (engine drops them, server stores them)
    is implicit; a future refactor forwarding `HavocSpec.server` to `engine.arm()` would
    silently lose them. Worth a comment/assert pinning the contract.

12. **gap - `/quotes` ignores its query and always returns `[]`** (main.rs ~205-212).
    Documented as trades-only, but a client querying a nonexistent symbol or malformed
    regime gets `200 OK []` rather than any signal, diverging from `/trades` which parses
    and validates.

13. **gap - History generation runs synchronously on the async handler thread** (main.rs
    ~228-243). `merged.next_tick()` never returns `None` (infinite generator); a no-`end`,
    `limit == MAX_HISTORY_LIMIT` request always synthesizes 1000 ticks of CPU work inline
    with no `spawn_blocking`, blocking a tokio worker (fast today, <250ms, but a blocking-
    in-async pattern).

14. **nit - `limit == 0` handling split across two functions** (main.rs ~196-199,
    ~221-223). Redundant zero handling; no real bug.

15. **nit - `arm_divergence` never validates `ms` and there is no clear/reset path**
    (main.rs ~144-159, ~150-156). An absurd `GoDark { ms: u64::MAX }` saturating-muls into
    a far-future `dark_until_ns`, bricking all outbound frames forever with no clear short
    of restart. Arming an engine divergence afterward does not reset stale `delay_ms`/
    `dark_until_ns` - the cross-contamination the engine comment (lib.rs ~131-134) guards
    against on its side has no server-side equivalent.

> Most actionable in E: #3 (BoundedSeek pre-`start` ticks break the cursor contract), #4
> (ms-truncation pacing), #2 (GoDark drops vs delays) plus #1 (global), and #5/#6
> (overlap/resubscribe double-feeds violating PollCursor ordering).

### F. adapter test suites (`adapter_smoke.rs`, `data_client_transport.rs`, `havoc.rs`)

1. **bug (test) - Smoke test asserts only event variants, never payloads**
   (`adapter_smoke.rs` ~198-213). Four `assert!(matches!(...))` check only the variant of
   each event, never order-id/qty/price/venue. The stub hard-codes `O-1`, `100.00`, `1`,
   balance `9900`, position `1` - none asserted. A bug mapping the fill to the wrong
   order/qty/price, or dropping the venue order id, still produces a `Filled` variant and
   passes. "Drives live exec events" proves only that four variants arrive in sequence.

2. **bug (test) - Subscribe assertion checks only `instrument_id`**
   (`data_client_transport.rs` ~184-187). The pushed trade carries `price:"100.00"`,
   `size:"1"`, `aggressor:"Buyer"`, `ts_event:10`; none asserted. A conversion bug (wrong
   price scaling, flipped aggressor, dropped ts) passes. The integration-gate test asserts
   less than the havoc tests do.

3. **bug (test) - `request_trades` asserts only `data.len() == 2`**
   (`data_client_transport.rs` ~209-211). The two trades have distinct prices/sizes/
   aggressors; none of the contents or order is checked. Two copies of the same wrong
   trade passes.

4. **bug (test) - Latency lower bound measures setup, not the filter** (`havoc.rs`
   ~428-440). `start` is captured before `subscribed_data_client` runs connect + seed +
   handshake + subscribe. `elapsed >= 50ms` cannot distinguish "latency havoc added 50ms"
   from "setup alone took 50ms and latency added 0." If `data_nanos` were silently
   dropped, the test could still pass on setup overhead. The comment treats setup as an
   upper-bound concern only; it weakens the lower bound too.

5. **bug (test) - Quota tolerance allows sub-interval spacing** (`havoc.rs` ~710-716).
   `max_requests_per_second: Some(2)` -> `min_interval = 500ms`, but the assert requires
   `gap >= 450ms` - 50ms slack below the true interval. A throttle shipping at 460ms
   (faster than 2/sec) passes. Tolerance is on the wrong side: should be `>= 500ms` with
   slack only for larger gaps.

6. **bug (test) - Seeded-RNG determinism is untested** (`havoc.rs` ~445-525). All
   probabilistic tests use `prob: 1.0` (or `0.0`), so `seed: Some(1)` is never load-
   bearing (`draw()` short-circuits, `< 1.0` always true). No coverage of intermediate
   probability (e.g. 0.5 with a pinned seed -> deterministic sequence). The seeded-RNG
   contract (the whole point of `seed` and `client_havoc_for_dispatch` XOR-ing the
   counter) is untested at the integration level - exactly where a broken `draw` would
   hide.

7. **bug (test) - `conn_reconnect_respects_max_attempts` exact-count is racy** (`havoc.rs`
   ~566-606). Asserts `handshakes == 3` after `timeout(2s) + sleep(300ms)`. Brittle under
   scheduler delay; the real contract is "disconnected after cap," which a lower+upper
   bound or an `is_disconnected` check would express more robustly.

8. **gap - `havoc_drop_prob_one_drops_all` can pass for the wrong reason** (`havoc.rs`
   ~443-462). One trade + assert-nothing-arrives can't distinguish "drop applied" from
   "trade never delivered for an unrelated reason." Interleave a guaranteed-undropped
   control event, or push N and assert zero of N.

9. **gap - HTTP polling test doesn't assert the poll repeats**
   (`data_client_transport.rs` ~260-264). Asserts `ws_hits == 0` (valuable) but only that
   one polled trade arrives; a one-shot poll that fired once and stopped passes. Repeated
   polling is the polling profile's defining behavior.

10. **gap - `conn_idle_timeout_triggers_reconnect` only asserts `handshakes >= 2`**
    (`havoc.rs` ~540-564). Doesn't assert the client recovered (re-subscribed, usable) or
    that it stops reconnecting; a reconnect storm also satisfies `>= 2`.

11. **gap - `conn_heartbeat_pings_when_enabled` only asserts `pings >= 1`** (`havoc.rs`
    ~608-629). Doesn't assert pacing at `heartbeat_interval_ms` or that the client handles
    the response. The inbound server-`Ping` -> client-`Pong` path (`lifecycle.rs`
    ~225-229) is never exercised (stub counts `Ping` but never sends one).

12. **gap - `ships_server_havoc` doesn't assert payload values round-tripped** (`havoc.rs`
    ~334-402). Asserts the two control bodies contain `"RejectNextSubmit"`/`"GoDark"`
    substrings but not `reason:"nope"` or `ms:25`. A serialization bug shipping the wrong
    duration/empty reason passes. Fragile type-name substring coupling.

13. **gap (product-relevant) - Claimed divergence surfaces have no behavioral test**
    (`havoc.rs` whole file). The header claims "partial fills, rejects, delays, duplicate
    fills, dropped account updates, blackouts." Actually covered: server-havoc shipping,
    latency, drop, duplicate (trade ticks only), reorder, clean, idle-reconnect, max-
    attempts, heartbeat, single-connection, quota, request-timeout-reject. **Missing:** no
    partial-fill test, no duplicate-*fill* test (only duplicate trade ticks), no dropped-
    *account-update* test, no end-to-end `GoDark` blackout-suppresses-the-stream test
    (only checked as a shipped control body).

14. **smell - Triplicated stub harness with subtle divergence** (`adapter_smoke.rs` ~34,
    `data_client_transport.rs` ~50, `havoc.rs` ~48). The HTTP/WS stub is copy-pasted across
    all three; the smoke/transport copies use a "read one request" model that does NOT
    parse `Content-Length` for POST bodies, whereas `havoc.rs` has a proper
    `read_request`/`content_length` reader. A fix to one is easily missed in the others.

15. **smell - WS stubs match on type-name substring, not parsed `ClientMessage`**
    (`adapter_smoke.rs` ~96-108 `"SubmitOrder"`, `data_client_transport.rs` ~131
    `"Subscribe"`, `havoc.rs` ~214). A wire-format change (field rename, envelope change)
    leaves the stub matching the type-name substring so the test passes while the protocol
    broke.

16. **smell - `havoc_duplicate_prob_one_doubles` doesn't assert "exactly two"** (`havoc.rs`
    ~466-484). Asserts `ts_event` equal (both constant `10`); a triple-emit passes, and
    there's no tail drain check. Doesn't assert the two are the same trade vs two
    independently-generated trades.

17. **smell - Reorder test never exercises the odd-count `flush` path** (`havoc.rs`
    ~504-525). Feeds exactly two trades so the held message always pairs; if
    `HavocFilter::flush` lost the dangling held message at stream end, no test would catch
    it.

18. **smell - Quota "wait until 3 requests" loop has no timeout** (`havoc.rs` ~697-708).
    If polling silently stopped after the first GET, the loop spins forever and the test
    hangs rather than failing clearly - unlike every other wait in the file.

19. **smell - `conn_http_request_timeout_rejects_order` doesn't correlate the reject**
    (`havoc.rs` ~763-827). Asserts a `Submitted` then `Rejected` arrives, but not that the
    reject's reason/order-id matches the timed-out order; a mismatched-id reject passes.

20. **smell - All tests are `#[ignore]` + `current_thread`** (all three files). `brokkr
    check` never runs them, so the default CI gate exercises none; combined with the
    variant-only assertions, the suite gives less regression protection than its size
    suggests.

21. **nit - Documented `brokkr test` invocation selects zero tests**
    (`data_client_transport.rs` ~18). The doc says `... data_client --debug`, but neither
    test fn name contains `data_client` (the substring matches the file name, not a test
    name), so the name-filter selects nothing.

22. **nit - `cached_order` duplicated byte-for-byte** between `havoc.rs` ~719-752 and
    `adapter_smoke.rs` ~116-149; a divergence between the copies would silently change
    what each test exercises.

23. **nit - `data_havoc`/`conn_havoc` helpers build `HavocSpec` inconsistently** (`havoc.rs`
    ~273-287); one uses `ConnHavoc::default()`, the other `..HavocSpec::default()` - invites
    a future test to forget a field.

> Product bugs the tests fail to guard: the `flush`/held-message path (odd-count reorder),
> the inbound `Ping`->`Pong` reply path, and `GoDark`/blackout end-to-end tolerance are all
> unexercised. Most load-bearing test fixes: F.4 (latency lower bound), F.5 (quota
> tolerance), F.1/F.2/F.3 (variant/count-only assertions that pass a mis-mapped fill/trade).

---

## Wave 3 - duplicated business logic / consolidation

Merged from three lenses: G (adapter+server), H (protocol+engine+data), I
(whole-workspace cross-crate). Where all three converged on one item it is listed
once with all sites. Each entry: WHAT is duplicated, WHERE, whether copies have
DRIFTED, and the proposed consolidation.

### Cross-crate (the high-value ones - hoist into `mogwai-protocol`, the shared dep)

1. **dup - UNIX-nanos "clock now" helper, two/three copies**. `now_ns() -> u64`
   (server main.rs ~100) and `now_unix_nanos() -> UnixNanos` (adapter client.rs ~1401)
   share a byte-identical body (`duration_since(UNIX_EPOCH).expect("clock before
   epoch").as_nanos() as u64`). Not drifted in logic; server returns bare `u64`, adapter
   wraps in `UnixNanos`. The engine correctly takes `ts` as a parameter and is NOT a third
   site. **Consolidate:** `pub fn now_unix_nanos() -> u64` in `mogwai-protocol`; server
   calls it, adapter wraps. (Note: this is also the locus of bugs A.6 / E.10 - the panic on
   clock skew and the `u128 as u64` truncation; fix once, here.)

2. **dup - Decimal<->f64 conversion with OPPOSITE failure policies (drifted, hazardous)**.
   The adapter PANICS: `d.to_f64().expect("decimal fits f64")` at convert.rs ~18/22 and
   open-coded at client.rs ~1786, ~2217, ~2257-2259. The data crate SATURATES:
   `decimal_to_f64` -> `unwrap_or(0.0)` (generated.rs ~671) and `decimal_from_f64` ->
   MAX/MIN/ZERO (~658), with a documented rationale. Same concept, opposite robustness
   contract, silently. The adapter's panic sits on the hot inbound fill/balance path and on
   control-plane/`/orders` input that can be fuzzer/attacker-influenced - a pathological wire
   Decimal crashes the runtime instead of degrading. (This is the root of bugs A.8 and D.1.)
   **Consolidate:** hoist `decimal_to_f64`/`decimal_from_f64` into `mogwai-protocol`, make the
   adapter use the saturating form (or make the panic a documented, deliberate choice), and
   add `convert::money(d, currency) -> Money` to fold the five open-coded `Money::new(...
   to_f64 ...)` sites (G.dup4/dup5). Note the live-fill path maps zero commission to `None`
   while the report path emits `Money(0.0)` (client.rs ~2213 vs ~1785) - an undocumented
   divergence to pin down while consolidating.

3. **dup - `ServerMessage` exec-vs-data classification, two divergent predicates (drifted,
   semantic)**. Server `is_execution_event` (main.rs ~361) = "not Trade and not Quote", so
   `AccountState` counts as execution and is delayed under `DelayAcks`. Adapter `event_kind`
   (client.rs ~1353) buckets `AccountState` as `EventKind::Data`, delayed by the *data*
   latency knob, not the exec one. Same message, opposite category on the two ends. A test
   arming both server `DelayAcks` and client `exec_event_nanos` will see `AccountState`
   delayed server-side but bucketed as data client-side. `EventKind` already lives in
   protocol (~252) but only the adapter uses it. **Consolidate:** `ServerMessage::category()
   -> EventKind` in protocol; both the server delay gate and the adapter `event_kind` consume
   it. Decide once whether `AccountState` is exec or data.

4. **dup - Engine has a whole `Instrument` struct that mirrors protocol `InstrumentDef`**.
   `mogwai_engine::Instrument` (engine lib.rs ~31-39) has the same seven fields as
   `InstrumentDef` (protocol ~260-268), plus a hand-written `From<&Instrument> for
   InstrumentDef` (~41). The engine adds no behavior to it. Any new `InstrumentDef` field must
   be added in three places or it silently drops. **Consolidate:** delete
   `mogwai_engine::Instrument`, use `InstrumentDef` directly (the engine already path-deps
   protocol). Removes a struct, a `From`, and a drift surface.

5. **dup - Hardcoded BTCUSDT instrument (precision 2/8, increments 1e-2/1e-8) in 3+ places
   (coupled, barely guarded)**. Engine seed (engine lib.rs ~95-103), server `scalars_for`
   pins `modal_tick=1e-2, price_decimals=2` to match (server source.rs ~34-39), and adapter
   test fixtures (convert.rs ~113, client.rs ~2365). The server test
   `btcusdt_uses_engine_price_grid` (source.rs ~133) exists *only* because this constant is
   duplicated and must stay in lockstep - change the engine precision to 1 and the generated
   grid and engine fill grid silently diverge, caught by that one test. **Consolidate:**
   define the default instrument set once in protocol (`pub fn default_instruments() ->
   Vec<InstrumentDef>`); engine seeds from it, server `scalars_for` reads `price_precision`
   from the def instead of re-pinning `2`.

6. **dup - Default request-timeout "30s" lives in a protocol doc-comment but is implemented
   in the adapter, repeated 3-4 times**. Protocol comment says "0 keeps 30s" (lib.rs ~88);
   implemented at client.rs ~1342 and hardcoded as the `HttpClient::new` arg at ~85 and ~1452.
   **Consolidate:** `const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;` in protocol, referenced
   everywhere.

7. **smell - Two different `/trades` caps on the same scan**. Adapter `capped_limit` clamps to
   `HISTORY_LIMIT_CAP = 10_000` (client.rs ~57), but the server re-clamps to `MAX_HISTORY_LIMIT
   = 1_000` (server main.rs ~40), so the server always wins and the adapter's 10k ceiling is
   dead - a caller asking for 5_000 silently gets 1_000. **Consolidate:** one shared constant
   (protocol), or make the adapter cap <= the server cap with a comment.

8. **nit - RNG seeding idiom `seed.map_or_else(from_entropy, seed_from_u64)` repeated**
   (adapter client.rs ~1270, lifecycle.rs ~142; the generator at generated.rs ~321 is always
   seeded - correct asymmetry). A tiny `seeded_rng(Option<u64>) -> StdRng` helper would remove
   the two identical adapter sites. Barely worth a shared crate; flagged for awareness. Also
   confirm (per D.6) the reconnect-jitter RNG is actually seeded from the havoc seed.

9. **smell - "mean inter-arrival gap" computed three times across two languages**.
   `smoke.py:107 mean_event_gap`, server `source.rs ~161 mean_duration`, data `generated.rs
   ~1170 durations`. And `scripts/smoke.py` (~71-90, ~182-380) hand-maintains protocol message
   / divergence / regime JSON shapes (tag names, decimal-as-string convention) in untyped
   Python - only the runtime smoke test catches a field rename. Inherent to a stdlib-only
   harness; add a comment in smoke.py pointing at `mogwai-protocol` as the source of truth.

### Within `mogwai-adapter`

10. **dup - Triplicated HTTP/WS test-stub harness (~200+ lines, DRIFTED)**. `run_stub`,
    `respond_json`, `serve_ws`, `instrument_id()`, `INSTRUMENTS_JSON`, `cached_order()`,
    `next_exec_event()` are copy-pasted across `adapter_smoke.rs`, `data_client_transport.rs`,
    `havoc.rs`. Real drift: only `havoc.rs` parses `Content-Length`/reads the request body;
    `respond_json` is `(stream, body)` in two files but `(stream, status, body)` in havoc;
    `serve_ws` is fixed-frame in two but data-driven from `StubState` in havoc;
    `next_exec_event` timeout is 2s vs 3s. The havoc variants are a strict superset.
    **Consolidate:** a shared `tests/common/mod.rs` owning the havoc.rs superset; the other two
    adopt the data-driven stub. Largest single duplication in the workspace.

11. **dup - Validation block duplicated across both adapter configs**. `MogwaiDataClientConfig`
    and `MogwaiExecClientConfig` each call `validate_client_havoc + validate_conn_havoc +
    validate_market_regime` over the same `Option<HavocSpec>` (config.rs ~50-56 and ~113-119),
    byte-identical. **Consolidate:** `HavocSpec::validate(&self)` in protocol (it already owns
    the sub-validators); both configs collapse to one call.

12. **smell - Four near-identical havoc dispatch/flush wrappers (~50 lines)**.
    `dispatch_market_havoc`/`flush_market_havoc` (+`_shared`) (client.rs ~759-807) and
    `dispatch_exec_havoc`/`flush_exec_havoc` (+`_shared`) (~1906-1932) differ only in the
    handler closure and context tuple. **Consolidate:** generic `dispatch_havoc<H:
    Fn(ServerMessage)>` + `flush_havoc` + one generic `_shared`.

13. **smell - Enum-boundary mapping family split across files**. `wire_side`/`wire_order_type`/
    `wire_time_in_force` (client.rs ~2311-2335) are the same "match-and-bail nautilus<->wire"
    shape as `convert::aggressor` (convert.rs ~25-31) but live in a different module.
    **Consolidate:** move the three `wire_*` fns next to `aggressor()` in convert.rs.

14. **smell - `"MOGWAI"` venue literal scattered**. `MOGWAI_VENUE` (lib.rs ~22) is canonical,
    but factory `name()` returns the bare `"MOGWAI"` literal (factories.rs ~59, ~117) and two
    test files use `Venue::from("MOGWAI")` (data_client_transport.rs ~143, havoc.rs ~233) while
    adapter_smoke.rs and the unit tests correctly use `*MOGWAI_VENUE` - the tests are already
    inconsistent. **Consolidate:** `pub const MOGWAI_VENUE_STR: &str = "MOGWAI"` in lib.rs,
    derive `MOGWAI_VENUE` from it, source factory names from it, switch the two test sites to
    `*MOGWAI_VENUE`.

### Within the core crates (protocol / engine / data)

15. **dup - Generator literals duplicated across both constructors (not yet drifted)**.
    `start_price = 60_000`, `typical_size = 0.1` (`Decimal::new(1,1)`), `vol_scalar = 5e-8`
    appear verbatim in both `from_fingerprint_medians` (generated.rs ~223-225) and
    `xbtusd_anchor` (~237-239). **Consolidate:** hoist to module consts, or a private
    `with_price_axes(...)` helper so each public constructor supplies only what differs.

16. **smell - No `validate_divergence`, unlike the other two havoc surfaces (enables a bug)**.
    `validate_conn_havoc` and `validate_market_regime` exist and are called from server+adapter,
    but `control::Divergence` (protocol ~818) has none. `PartialFillNext.fraction` is an
    unbounded `Decimal`; negative or `>1` flows into `last_qty = quantity * fraction` /
    `leaves_qty = quantity - last_qty` (engine ~200-201), yielding negative `last_qty`/
    `leaves_qty`. This is the validation-family asymmetry AND the mechanism behind bugs B.2
    (zero-fraction spurious fill) and B.3-adjacent. **Consolidate/fix:** add
    `validate_divergence` enforcing `0 <= fraction <= 1`, called from the same sites as the
    other two validators.

17. **dup - Finite-range check idiom `x.is_finite() && (lo..=hi).contains(&x)` repeated 6x
    (minor drift)**. protocol VolStorm/LiquidityDrought/SessionEdgeSpike/ReopenGap arms
    (lib.rs ~157/164/178/192), the negated backoff-factor form (~109), and adapter config.rs
    ~165 which drops the `is_finite()` guard entirely. VolStorm uses `(0.0..=100.0) && >0.0`
    (clumsy half-open) while SessionEdgeSpike uses the same range *including* 0.0 - identical-
    looking checks, different lower bounds on purpose. **Consolidate:** `finite_in(v, lo, hi)`
    (+ an excl-lower variant) in protocol; validators supply the message. Route config.rs ~165
    through it so the finite guard is uniform.

18. **smell - `MarketRegime` destructured in two crates that must stay in lockstep**.
    `validate_market_regime` (protocol ~155-198) and `RegimeState::new` (data generated.rs
    ~483-518) are coupled parallel match arms - a new variant or renamed field needs both, with
    no compiler link forcing the validator to cover a new arm. SessionEdgeSpike's `start_hour <
    end_hour` invariant is enforced in the validator and re-checked at runtime (~530) trusting
    the validator ran. Coupling smell, not a copy to merge; guard against a variant slipping
    through one side.

19. **smell - Vol-multiplier composition: additive-within-regime vs multiplicative-across**.
    `RegimeState::vol_mult` does `self.vol_mult + edge_mult` (additive; generated.rs ~536) while
    `next_latent_mid` does `session.vol_mult * regime.vol_mult` (multiplicative; ~392), and the
    storm multiplier is also consumed at ~490. No literal dup, but a fragile-formula
    concentration whose only explanation (the "additive but base is 1.0" trick) lives in a
    protocol doc-comment (~138) one crate away from the data code it explains. (Same as C.5.)

20. **dup - `MinMedianMax` range-membership written twice**. `(range.min..=range.max).contains`
    in production `validate_f64` (generated.rs ~644) and test `assert_in_range` (~1323).
    **Consolidate:** `impl MinMedianMax { fn contains(&self, v: f64) -> bool }`, used by both.

21. **nit - `utc_hour_dow` destructure-and-discard-dow repeated 3x** (generated.rs ~189, ~529,
    ~1206). A `utc_hour(clock_ns)` wrapper removes the throwaway binding. Low value.

22. **nit - Engine hand-rolls `abs(Decimal)` / `same_sign`** (engine ~551-555). `Decimal::abs()`
    already exists; `same_sign` has no std equivalent and is fine to keep.

### Deliberate - do NOT merge (confirmed intentional)

- **Engine order/fill/position state machine vs the adapter `ExecState` mirror.** The mirror
  re-derives nautilus-side state from the wire stream - it is the structural reason mogwai is
  an external process. BUT add a one-line comment at client.rs ~2180/~2249: orders/fills are
  recomputed from events while positions are copied wholesale from `AccountState` snapshots, so
  under `DropNextAccountUpdate`/`DuplicateNextFill` the adapter's `filled_qty`/`avg_px` and
  positions diverge *by design* and are not reconcilable with the engine's by construction.
- **`convert::aggressor` and `instrument_any`** - single-copy nautilus<->wire boundary mappings.
- **`MergeSource` (server-side k-way merge) vs `PollCursor` (client-side poll dedup)** - adjacent
  but genuinely distinct concerns over the same ascending-`ts_event` invariant.
- **`update_bar_state`** is already single-sourced across live and historical bars (good).
- **The two `trade_id` schemes** (`symbol-ts` for market-data ticks vs `T-{seq}` for fills) are
  different id spaces sharing one wire field; not a true dup, but undocumented - worth a note.

---

## Top consolidation recommendations (highest value first)

1. **Create a shared home in `mogwai-protocol`** for: `now_unix_nanos`, saturating
   `decimal_to_f64`/`decimal_from_f64`, and a `ServerMessage::category()` classifier. One move
   fixes the clock-skew panic (A.6/E.10), the inbound Decimal panic hazard (A.8/D.1), and the
   exec/data classification drift (G.nit10/I) at once.
2. **Add `validate_divergence` to protocol** (item 16) - closes the validation-family
   asymmetry and the `PartialFillNext.fraction` bug (B.2).
3. **Delete `mogwai_engine::Instrument`; use `InstrumentDef`** (item 4), and single-source the
   default instrument set + `/trades` caps + request-timeout constants (items 5, 6, 7).
4. **Shared adapter test-support module** (item 10) - largest single duplication, ~200+ lines.
5. **`HavocSpec::validate` + generic havoc dispatch** (items 11, 12) and the `finite_in` helper
   (item 17).
