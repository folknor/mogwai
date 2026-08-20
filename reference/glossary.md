# mogwai glossary

- **Run**: one foreground mogwai process, many ledgers, many keyed rivers and
  as many boats as distinct (river, speed) seats. A run may declare a simulated
  duration, and so may an individual passenger.
- **River**: the generated market-data sequence for one resolved instrument
  shape, keyed by the requested symbol plus that shape's knobs. Rivers are
  created on first use and never serialize on each other's checkpoint chain.
  History reads a river directly; nothing has to be boarded for `/trades` or
  `/quotes` to answer.
- **Boat**: the paced reader of one river, placed on demand when the first
  socket boards and carrying its own `SimClock`, its own broadcast ring and its
  own market-reading memo. Sockets asking for the same river and the same speed
  share one boat; a different quantized speed places a second boat on the same
  water. One ledger still carries one cadence.
- **Boatyard**: the run-owned registry of keyed boats and the tickets that keep
  them alive. A boat winds down when its last passenger leaves.
- **Tape**: what a boat publishes - the paced frame stream broadcast to that
  boat's passengers only. The boot river's warmup is materialized before
  readiness; a river reached later materializes on first read.
- **Ledger**: one `mogwai-engine` instance, owned by one ACCOUNT and created on
  first sight of that account id. A run holds as many as it has accounts and
  they share nothing: positions, balances, order history and armed divergences
  are all per ledger. Every socket a client opens under one account id acts on
  that account's ledger, whatever symbol each bound, so a client trading two
  instruments is trading one book. Order entry is WebSocket-only - there is no
  HTTP order carrier. A ledger OUTLIVES the connection that named it, which is
  what makes a reconnect a continuation.
- **Boot symbol / boot river**: the shape the run boards a boat on before it
  writes its readiness line, and the river a request that names no symbol binds.
  It is the only river warmed eagerly and the only boat that never winds down;
  every other river is boatless until someone boards it.
- **Warmup**: the uniformly servable simulated history from `data_origin_ns`
  through `run_start_ns`. `warmup_ns` is their distance. The boot river is
  materialized before readiness and every other river on first read.
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
- **ReadyRecord**: one versioned JSON line describing the venue, its only stdout
  output. It names no symbol; attach identity is `addr` plus `run_seed`.
- **RunComplete**: the terminal WebSocket announcement for a planned duration
  completion, followed by a normal close. A socket may carry its own
  `duration_ms`, measured in simulated milliseconds on its boat's clock from its
  own boarding instant, so passengers on one boat complete independently.
- **Served symbol**: any symbol a request names that resolves to a legal,
  fundable shape. A symbol with a preset gets that preset's shape; one without
  gets the default shape under its own label, memoized per run. Refusals are
  about the label or the run's `[balances]`, never about the absence of a
  preset.
