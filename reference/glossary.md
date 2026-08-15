# mogwai glossary

- **Run**: one foreground mogwai process, one ledger, one paced tape and many
  keyed rivers. A run may declare a simulated duration.
- **Tape**: the run-owned generated market-data sequence. It is warmed before
  readiness and broadcast to every connected WebSocket.
- **Ledger**: the single `mogwai-engine` instance owned by the run. HTTP and
  WebSocket order entry act on the same ledger.
- **Warmup**: the materialized simulated history from `data_origin_ns` through
  `run_start_ns`. `warmup_ns` is their distance.
- **Instrument class**: the shape an instrument takes - `spot` (a base/quote
  currency pair) or `future` (cash-settled, continuous, no expiry or roll).
  Everything else about the instrument is a knob on top of it.
- **Multiplier**: currency units per 1.0 of price. `1` for spot, `2` for MNQ,
  `5` for MES. Notional is `qty * px * multiplier` everywhere.
- **Tick value**: the currency value of one price increment,
  `price_increment * multiplier`. Derived, never configured, so it cannot
  contradict the two numbers it comes from. MNQ reads 0.50, MES 1.25.
- **Contract size grid**: how the generator turns a notional target into a
  printable size. Spot is a 1e-8 grid; a future is whole contracts floored at
  one, which is what keeps a precision-0 quantity from being zero.
- **Posted margin**: the collateral the account currently has committed on one
  instrument - maintenance for the open position, initial for the resting
  non-reduce-only orders. Account state the venue is authoritative for, as
  distinct from the instrument's margin parameter, which stays server-side.
- **Variation margin**: the daily settlement transfer. At the settlement
  instant the accumulated difference between the settlement price and the
  position's VWAP moves in actual cash, and the VWAP resets to that price.
- **Session calendar**: the weekly open windows in exchange-local time. A
  scheduled close is configuration and the market is genuinely shut inside it,
  as distinct from `ReopenGap`, which is unscheduled havoc.
- **ReadyRecord**: one versioned JSON line, the venue's only stdout output.
- **RunComplete**: the terminal WebSocket announcement for a planned duration
  completion, followed by a normal close.
