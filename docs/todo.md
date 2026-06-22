# mogwai - TODO

A fake broker/exchange that plugs into **broadarrow** to exercise the *live*
trading path: it replays Kraken trade history as market data and emits the messy,
realistic execution divergences (partials, rejects, delays, drops, blackouts) an
in-process sandbox structurally cannot produce. mogwai never imports nautilus; the
client-side `ExecutionClient`/`DataClient` impls live in broadarrow and speak this
crate's protocol over the wire.

## Status

Done and verified end-to-end (`scripts/smoke.py`, 8 unit tests):

- `mogwai-protocol` - native JSON-over-WS wire types + `control::Divergence`.
- `mogwai-engine` - venue-agnostic core with the divergence-injection seam
  (`PartialFillNext`, `RejectNextSubmit` implemented).
- `mogwai-data` - streaming Kraken CSV loader (O(1) memory over multi-GB files),
  seconds→ns, k-way `MergeSource`; verified on the real 43GB dump.
- `mogwai-server` - axum `/health`, `/ws` (orders + market-data replay),
  `/control/divergence`. Replay runs on a blocking thread with backpressure;
  speed via `MOGWAI_REPLAY_SPEED`.

## Next

### 1. Replay realism
- [ ] Start-time / windowing control - replay from a given date, not epoch start
      (otherwise the full history floods instantly when unthrottled).
- [ ] `Unsubscribe` actually stops a running replay (today it's a no-op; the
      replay thread has no cancellation path).
- [ ] Cap or document pathological inter-tick gaps under paced replay (multi-year
      gaps → multi-day sleeps at low speed).
- [ ] Optional aggressor inference via tick rule, as a `Permutation`.

### 2. Remaining divergences (server-side; timer/socket layer now exists)
- [ ] `DelayAcks { ms }` - delay outbound execution events.
- [ ] `DuplicateNextFill` - emit the next fill twice.
- [ ] `DropNextAccountUpdate` - induce account drift.
- [ ] `GoDark { ms }` - venue blackout.

### 3. Engine depth
- [ ] `generate_account_state` - track balances/positions, not just orders
      (needed before the broadarrow adapter can reconcile).
- [ ] Real matching against an order book (today: immediate fill, no book).
- [ ] `ModifyOrder` handling (today: no-op).

### 4. Protocol gateways
- [ ] Strategy A: `mogwai-proto-binance` - Binance-shaped REST+WS over the same
      engine, so nautilus's existing Binance adapter connects via its
      `base_url_*` overrides (zero new adapter).

### 5. broadarrow side (separate workspace)
- [ ] `ExecutionClient` + `DataClient` impls consuming this protocol.
- [ ] `venue.rs` adapter arm + `core::venue` PROFILES row (build-time guarded).

## Notes / gotchas
- Build target is redirected to `/media/folk/Banan/cargo`; `cargo test` does not
  emit the runnable binary - use `cargo build --bin mogwai-server`.
- Kraken history is **trades only** - no quotes, no L2, no aggressor side
  (`AggressorSide::NoAggressor`). Symbol comes from the filename (`XBT` = BTC).
- Data dir: `MOGWAI_DATA_DIR` (default `/media/folk/Banan/Kraken_Trading_History`).
- `research/` (nautilus clone, ~413MB) is gitignored.
