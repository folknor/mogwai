# TODO

Open work only. How the built system works lives in
`reference/architecture.md`; the landing-by-landing history is in git; the
per-crate mechanics are in code comments.

Once an item here is completed, it GETS REMOVED ENTIRELY. If the prose contains
any relevant information that must endure, it gets either (a) added as an inline
comment in the code, or (b) added to an existing or new ../reference/ document.

Or both. There are no exceptions.

## Open issues

- The accelerated smoke (a coherent-clock-spec gate, not a forward-origin gate)
  times out on its market-data subscribe.

- mogwai-engine `next_position` unbounded accumulation. The per-fill weighted-
  average is now overflow-guarded (a single oversized order is rejected before
  it reaches the arithmetic), but `current.qty` still accumulates across many
  individually-valid orders on one symbol/side, so a long-lived engine can
  overflow the `current_abs * avg_px + delta_abs * px` computation over time.
  Closing it means introducing a position-size or notional cap - a design
  decision, not a local fix.

## Bug-hunt findings (2026-07-01)

Sub-agent-per-crate hunt, ranked most-severe-first within each crate. The
mogwai-protocol, mogwai-engine and mogwai-data findings have been verified and
either fixed, judged not-real, or documented in code, and are removed from here.
The mogwai-server and mogwai-adapter sections below remain open and unverified -
verification happens before/while fixing.

### mogwai-server

1. bug - `src/main.rs:1198-1203` (writer task): `DelayAcks`
   head-of-line-blocks the market-data feed. The session has a single mpsc channel
   and one writer loop; on an execution event the writer does
   `tokio::time::sleep(...).await` before sending, and while it sleeps it stops
   draining `rx`. Every market-data tick enqueued after that exec event is stuck
   behind it. `validate_divergence` allows `DelayAcks.ms` up to 3_600_000 (1h),
   and the taxonomy separates `DelayAcks` (delay execution acks) from `StallData`
   (stall data). Scenario: client subscribes to BTCUSDT, `POST /control/divergence`
   `{"type":"DelayAcks","ms":3600000}`, submits one order. The fill/AccountState
   enters the channel; the writer sleeps ~1h before sending, and every trade
   generated during that hour queues behind it (channel fills at 1024, replay
   threads park). Client sees the data feed freeze for up to an hour, then a
   burst - though `DelayAcks` should touch only execution traffic.

2. bug/smell - `src/main.rs:994-1007` (`stamp_market_price`) called at `:1019` and
   `:1303`; `src/source.rs:172-188` and `:202-222`: price-less MARKET order
   stamping runs unbounded synthesis inline on the tokio worker while holding a
   process-global `std::Mutex`. `current_price` -> `positioned_generator` locks
   the global `checkpoint_store()` mutex and runs `source_at_or_before`, which on
   a cold/deep index walks up to `MAX_HISTORY_SEEK_TICKS` (~190k ticks, ~100 ms).
   The `/trades` handler deliberately pushes the identical synthesis onto
   `spawn_blocking` (`:1068-1073`), but the market-order path runs it inline on
   the async worker and serializes it against every other market order and every
   seeked `/trades` request via the one global mutex. Scenario: a burst of
   price-less `POST /orders` (Nautilus MARKET orders carry no price) on a
   long-running accelerated session stalls the runtime worker and blocks
   concurrent `/trades`/order requests behind the global lock.

3. smell - `src/source.rs:173-175`
   (`.expect("checkpoint store mutex poisoned")`): the global checkpoint mutex
   poisons permanently, bricking all checkpoint-path requests. Synthesis
   (`CheckpointIndex::new`, `source_at_or_before`) runs while the `std::Mutex` is
   held; if any generator ever panics there (arithmetic, a pathological configured
   scalar/regime), the mutex is poisoned and every subsequent checkpoint-path call
   - price-less market orders, seeked `/trades`, every live `Subscribe` - panics
   on `.expect(...)`. For the WS/order path this runs on the async worker inline,
   so it repeatedly kills request tasks. One transient panic becomes a permanent
   partial outage.

4. smell - `src/main.rs:1483-1506` (both branches call `now_ns()`), backed by
   `mogwai-protocol/src/lib.rs:41-45` (`now_unix_nanos` = `SystemTime`): replay
   pacing uses the wall clock (`CLOCK_REALTIME`), so an NTP/leap step stalls or
   bursts the feed. Every pacing sleep is `deadline_wall - now_wall` (accelerated)
   or a gap sleep re-derived from wall reads (identity). A backward wall step makes
   `deadline - now` huge -> the thread sleeps far too long (feed stalls); a
   forward step makes `deadline <= now` -> the thread bursts ticks with no pacing.
   A monotonic clock for the pacing delta would avoid this.

5. smell - `src/main.rs:1489-1506`: identity-mode (1x) pacing accumulates drift.
   The identity branch sleeps the per-gap delta measured from the previous tick's
   `ts_event` but never subtracts the time already spent generating/sending, so
   each tick's realized spacing is `gap + generation + scheduling` and over a long
   1x session the feed lags real wall-clock progressively. The accelerated branch
   avoids this by pacing to absolute deadlines; identity does not.

6. gap - `src/main.rs:1276-1292` with the seek in `src/source.rs:314-331`:
   re-subscribe seam can duplicate or drop a tick. `quiesce_replay` cancels and
   joins the old per-symbol thread but does not purge ticks that thread already
   enqueued into the shared channel; the new thread re-seeks the same
   deterministic tape to a freshly-sampled `sim_now`. If the old thread's last
   enqueued tick sits at `L` and the new seek target lands exactly on `L`, the
   client receives that trade twice; more commonly the ticks in `(L, sim_now)` are
   skipped (a gap). Nanosecond-tight, low probability, but the "join guarantees no
   stale tick at the seam" comment is not fully true for already-buffered ticks.

7. smell - `src/main.rs:1338-1341`: disconnect teardown can linger for the full
   `DelayAcks` window. On disconnect the handler drops `tx` and `writer.await`s; if
   the writer is mid-sleep on a large armed `DelayAcks` (up to ~1h identity) for an
   exec event, the connection task cannot finish until that sleep completes. The
   socket is already gone so the write fails immediately after - but only after the
   sleep. Per-connection task lingers well past the client's departure.

8. nit - `src/main.rs:1352-1355`: heartbeat fires immediately on connect.
   `tokio::time::interval(...).tick()` completes immediately on the first call, so
   a `Heartbeat` frame is emitted at t=0 the moment the socket opens, before any
   `Subscribe` and one full interval early. Cosmetic cadence wart.

9. wire-up (carried over from the mogwai-protocol wave-1 fix) - `mogwai-protocol`
   now exposes `validate_submit_order`/`validate_modify_order`, but no caller
   invokes them yet. The order-entry decode path (`POST /orders` plus the WS
   `SubmitOrder`/`ModifyOrder` handlers) should call them the same way it already
   calls `validate_divergence`/`validate_market_regime`, rejecting degenerate
   frames (non-positive quantity, priceless limit, a both-None modify no-op) at
   the protocol boundary instead of leaning on the engine's defensive rejects.

Lateral:
- smell - `mogwai-protocol/src/lib.rs:112-113` `wall_duration` floors to 1 ns,
  but the server's `DelayAcks` path feeds it through
  `sim.wall_duration(sim_duration_from_millis(delay))`: under a heavily
  accelerated clock a multi-ms delay can scale below ~1 ms tokio timer
  granularity and coalesce to effectively zero, so `DelayAcks` becomes a no-op at
  high `speed`. Probably acceptable, but worth being aware of.
- observation - engine tokio `Mutex` does not poison on panic (unlike the
  checkpoint `std::Mutex`), so a panic inside `Engine::process` reached from
  `/orders` or `/ws` tears down only that one request/connection task and the
  runtime recovers - the checkpoint-store `std::Mutex` (finding 3) is the
  asymmetric weak point.

Clean: no deadlocks or lock-held-across-await on the engine mutex
(`state.engine.lock().await` guards are statement-temporaries dropped before any
subsequent `tx.send(...).await`, verified at `:1304-1313` and `:1018-1025`); the
PID-file flock lifecycle (shared open-file-description across `fork`,
`mem::forget` in the parent, auto-release on child exit) is correct; stale-PID
reuse is structurally prevented because `stop`/second-`serve` decisions key off
the advisory flock, not the PID value.

### mogwai-adapter

1. bug (panic) - `src/client.rs:2957`: `AccountBalance::new` panics when
   `locked + free != total`. `handle_account_state` books each wire balance with
   the panicking `AccountBalance::new`, not `new_checked`. Nautilus
   (`research/.../types/balance.rs:82`) hard-asserts
   `locked.checked_add(free) == Some(total)` and panics otherwise. Reachable two
   ways: (a) any havoc'd/messy `AccountState` where the three amounts are
   inconsistent, and (b) even a consistent decimal snapshot - the three fields are
   each independently pushed through `decimal_to_f64` then re-quantized to
   currency precision in `convert::money`, so fractional balances can round such
   that fixed-point `locked+free` no longer equals `total`. Scenario: server
   pushes `AccountState{ total: 1.005, locked: 0.5025, free: 0.5025 }` (or the
   initial `/account` snapshot at connect). During `connect()` this panics
   synchronously and fails the client; on the WS push path it panics the
   unsupervised reader task, killing the whole exec stream. Defeats the discipline
   `convert.rs` documents (routing Price/Quantity/Money through `new_checked`
   specifically to avoid panicking the exec task).

2. bug (panic) - `src/client.rs:2271`: `VenueOrderId::from("")` panics for a
   submitted-but-unaccepted order. In `generate_order_status_reports`, a record
   with no venue id falls back to `VenueOrderId::from("")`. Nautilus
   `VenueOrderId::new_checked` rejects the empty string
   (`research/.../identifiers/venue_order_id.rs:47` + `macros.rs:47` route
   `From<&str>` through the panicking `new`; the source's own `#[should_panic]
   "...was empty"` test confirms it). Scenario: broadarrow submits an order
   (mirror inserts it `Submitted`, `venue_order_id: None`) and runs reconciliation
   before `OrderAccepted` arrives. `Submitted` is open, so it passes the
   `open_only` filter and hits `VenueOrderId::from("")`, panicking the awaited
   report generator during reconciliation.

3. bug - `src/client.rs:2411-2424` (`reject_for`) + `:2553`: a failed cancel is
   reported as a full order rejection. When orders ride HTTP (`HttpOrders`) and the
   `POST /orders` for a `Cancel` fails at transport, `reject_for` synthesizes
   `ServerMessage::OrderRejected`, and `handle_exec_message` then sets
   `record.status = OrderStatus::Rejected` and emits `OrderEventAny::Rejected`.
   Scenario: a live `Accepted`/`PartiallyFilled` order is canceled, the cancel POST
   times out - the mirror flips the still-live order to `Rejected` and emits an
   `OrderRejected` for it. Both a mirror corruption and an invalid nautilus state
   transition (Rejected is only valid from Initialized/Submitted), so nautilus
   drops/logs it and the real cancel failure is never surfaced as a cancel-reject.
   A modify failure correctly maps to `OrderModifyRejected`; cancel has no
   equivalent.

4. smell - `src/convert.rs:98`: `TradeId` collides for same-nanosecond trades.
   `TradeId::from(format!("{}-{}", t.symbol, t.ts_event))` makes the id a pure
   function of symbol+`ts_event`. The adapter's own `PollCursor` logic (and its
   tests) explicitly handle multiple trades sharing one `ts_event`, so collisions
   are expected in practice. Two trades at the same ns get identical `TradeId`s;
   nautilus doesn't validate market-data trade-id uniqueness, but any downstream
   dedup/persistence keyed on trade id silently drops the second trade. The
   exec-fill path is unaffected (it uses the wire `fill.trade_id`).

5. smell - `src/client.rs:2679`: `OrderUpdated` overwrites mirrored limit price
   with the wire `price` even when the amend omits it. `record.price = price`
   blindly takes the wire `Option<Decimal>`. If the venue echoes an amend that
   changed only quantity with `price: None`, the mirror loses the limit price. Low
   blast radius today because `record.price` is never read back into a report, but
   a latent correctness trap if reports later surface price.

6. nit - `src/client.rs:2483,2492`: report quantity precision guesses `8` when the
   instrument def is missing. `quantity_for_report`/`filled_quantity_for_report`
   default to precision 8 on a cache miss, so a report can carry a quantity at the
   wrong precision versus the real instrument. Minor, only on an unseeded cache.

Lateral:
- The `convert.rs` module header explicitly justifies routing every
  Price/Quantity/Money through `new_checked` "rather than panicking on a hostile
  wire value... the exec task that books commissions and balances must be able to
  drop a pathological amount rather than crash." The `AccountBalance::new` call at
  `client.rs:2957` violates exactly that invariant - it is the one place a wire
  value reaches a panicking nautilus constructor. Switching it to
  `AccountBalance::new_checked` and `filter_map`-dropping the offending currency
  (matching the surrounding `convert_amount` pattern) would close finding 1.
- `generate_account_state` (the public `ExecutionClient` method,
  `client.rs:1964`) also builds `NautilusAccountState` from caller-supplied
  balances; those come from nautilus so they're pre-validated, but it is the same
  panicking-constructor family, worth keeping in mind if callers ever pass raw
  balances.
- Both `MogwaiTimer::start` and the account-state path assume
  `try_get_*_sender`/runtime are present; the timer's `else { callback.call(event) }`
  branch invokes a nautilus `TimeEventCallback` directly from a spawned tokio task
  (no sender), which may violate nautilus's single-threaded callback assumption -
  only reachable when `try_get_time_event_sender()` returns `None`, so
  low-probability, but flagged since it's outside the exec/data hot path.

## Notes / gotchas

- The offline Kraken corpus is trades only - no quotes, no L2, no aggressor side.
  This shapes the offline analysis only; the running server synthesizes trades
  with a native `Buyer`/`Seller` aggressor and serves no quotes (`/quotes` is
  always empty). `KrakenCsvSource` and `TickRuleAggressor` survive in
  `mogwai-data` for the offline lineage and its unit tests.
- `MOGWAI_DATA_DIR` (default `/media/folk/Banan/Kraken_Trading_History`) is an
  offline-analysis input only (`analysis/`), never a server runtime knob.
- `research/` (the nautilus and broadarrow clones) is gitignored; read those APIs
  from there, depend on the sibling `../` checkouts.
