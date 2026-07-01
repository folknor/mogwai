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

- mogwai-adapter cancel-reject has no wire path. A failed cancel is now
  surfaced correctly as a nautilus `OrderCancelRejected` built locally in the
  adapter, but `mogwai_protocol::ServerMessage` still has no `CancelRejected`
  variant, so the reject bypasses the normal wire/havoc pipeline. Adding one
  would let cancel failures flow through the same path the fill/reject events
  already use.

- mogwai-adapter `tests/data_client_transport.rs` carries two ignored,
  deterministically-failing tests (`subscribe_and_request_drive_data_events`
  and `http_polling_subscribe_fetches_trades_without_ws`): the data client
  emits an `Instrument` event ahead of the expected `Trade` on connect/
  subscribe, which the tests do not tolerate. Pre-existing, surfaced during the
  bug-hunt - either the instrument-seeding-vs-trade-delivery ordering is wrong
  or the tests need to tolerate the leading `Instrument`.

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
