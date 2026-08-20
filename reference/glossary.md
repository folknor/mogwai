# mogwai glossary

The working vocabulary, split water-side (the tape and its machinery) from
account-side (the traders and their money). Each entry states what the word
means here and, where the meaning is load-bearing, the one fact that makes it
so.

## The venue and its modes

- **Venue**: one running mogwai process, serving the `MOGWAI` venue over
  loopback: one run, many rivers, many accounts. It gates on no symbol and
  admits any account id; both resolutions are total.
- **Server mode**: the venue launched by `mogwai serve` on the command line - a
  long-lasting exchange accepting connections from as many accounts as the user
  wants. The account id is the discriminator: no account sees any other, and
  strategies launched under the same account share that account's ledger
  regardless of instrument. Boot-time config - havoc knobs, budgets, speed -
  applies equally to every strategy run under an account. `mogwai serve`
  reports its connection parameters as one JSON line on stdout, and those go
  into the consumer's config explicitly. This mode exists as the path to
  running fifty-plus simulations at once under future performance work, where
  fifty-plus separate venue processes would choke.
- **Transient mode**: the venue launched by
  `mogwai_protocol::launch::launch(spec)` - an ephemeral venue the calling
  consumer owns, spawned when it is given no address (`ba forward --mogwai`
  on a config carrying no connection parameters is the known case). Nobody else ever
  connects, and the venue dies with the run that owns it (the kernel kills it
  when its launcher dies). Survives for convenience only: neither mode is a
  statement about isolation, determinism or test fidelity, and the venue's
  semantics are identical in both.
- **Run**: one foreground mogwai process, many ledgers, many keyed rivers and
  as many boats as distinct (river, speed) seats. A run may declare a simulated
  duration, and so may an individual passenger.

## The water

- **River**: the generated market-data sequence for one resolved instrument
  shape, keyed by the requested symbol plus that shape's knobs. A river's
  identity is everything that mutates the water - the resolved bundle, the
  seed, generator-level havoc - and never the delivery speed. Rivers are
  created on first use and never serialize on each other's checkpoint chain.
  History reads a river directly; nothing has to be boarded for `/trades` or
  `/quotes` to answer.
- **Boat**: the paced reader of one river, placed on demand when the first
  socket boards and carrying its own `SimClock`, its own broadcast ring and its
  own market-reading memo. Sockets asking for the same river and the same speed
  share one boat; a different quantized speed places a second boat on the same
  water, because speed is not part of river identity - it changes delivery
  cadence and never a generated value, so two boats at two speeds are reading
  one river. One ledger still carries one cadence. A boat is an implementation
  cache with no semantics of its own: the tape is deterministic and exogenous,
  so nothing a client can measure reveals whether it shares a hull.
- **Boatyard**: the run-owned registry of keyed boats and the tickets that keep
  them alive. A boat winds down when its last passenger leaves.
- **Tape**: what a boat publishes - the paced frame stream broadcast to that
  boat's passengers only. Materialization cost is paid at two different
  moments: the boot river's warmup is synthesized before the venue writes its
  readiness line, so its cost lands inside boot, while every other river does
  not exist until a socket bind or history poll first names it and is
  synthesized then - so the first requester of a non-boot symbol pays that
  river's warmup latency inside its own request.
- **Boot symbol / boot river**: the shape the run boards a boat on before it
  writes its readiness line, and the river a request that names no symbol
  binds. It is the only river warmed eagerly and the only boat that never winds
  down; every other river is boatless until someone boards it.
- **Warmup**: the uniformly servable simulated history from `data_origin_ns`
  through `run_start_ns`. `warmup_ns` is their distance, and every river owes
  the whole span before it can be served: it is what history queries answer
  from and where a strategy's warmup bars come from. When it is paid is the
  Tape entry's split - the boot river before readiness, every other river on
  first read.
- **Served symbol**: any symbol a request names that resolves to a legal,
  fundable shape. A symbol with a preset gets that preset's shape; one without
  gets the default shape under its own label, memoized per run. Refusals are
  about the label or the run's balances, never about the absence of a preset.

## The instruments

- **Instrument class**: the settlement shape an instrument takes, which is what
  decides how holding it moves the ledger. Five: `spot` (a base/quote pair -
  the base is held as a currency balance), `equity` (a share - held as a
  position, paid for in one currency, carrying lot size, borrowability and a
  settlement period), `future` (cash-settled, continuous, no expiry or roll),
  `perpetual` (a future that pays funding between long and short at an
  interval), and `inverse` (settled in the base asset, value `multiplier * qty
  / px`). Everything else about the instrument is a knob on top of its class.
- **Multiplier**: currency units per 1.0 of price. `1` for spot, `2` for MNQ,
  `5` for MES. Notional is `qty * px * multiplier` everywhere, except inverse,
  where value is `multiplier * qty / px`.
- **Tick value**: the currency value of one price increment,
  `price_increment * multiplier`. Derived, never configured, so it cannot
  contradict the two numbers it comes from. MNQ reads 0.50, MES 1.25.
- **Contract size grid**: how the generator turns a notional target into a
  printable size, derived from the instrument's own sizing. Spot is a 1e-8
  grid; a future is whole contracts floored at one, which is what keeps a
  precision-0 quantity from being zero.
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

## The accounts

- **Account**: an id plus everything the venue holds under it - its ledger, its
  risk state, its havoc arms. Created on first sight of the id and resolved
  totally: knobs the client named win, else a policy preset matching a
  requested name, else the default policy. The id is the client's, not minted,
  because a stable id is what makes a returning socket a continuation - and it
  is a bearer token: anyone who names it claims it, which is acceptable on a
  loopback venue and written down so it is not assumed to be more.
- **Ledger**: one `mogwai-engine` instance, owned by one account and created on
  first sight of that account id. A run holds as many as it has accounts and
  they share nothing: positions, balances, order history and armed divergences
  are all per ledger. Every socket a client opens under one account id acts on
  that account's ledger, whatever symbol each bound, so a client trading two
  instruments is trading one book. Order entry is WebSocket-only - there is no
  HTTP order carrier. A ledger outlives the connection that named it, which is
  what makes a reconnect a continuation.
- **Passenger**: the venue-side object for one account riding the run: the
  account id, its engine, its risk ledger, its freeze stamp and its seat. One
  per account, not per connection. Passengers on a river owe each other
  non-interference (which the tape's exogeneity gives - order flow never feeds
  back into the water) and invisibility (which attribution and per-account
  ledgers give: every order is claimed for its account - a venue-originated
  liquidation by the ledger that produced it - so every frame is delivered to
  the account it concerns).
- **Account policy**: the rules an account is enforced under - opening balance
  plus risk rules, named inline or as a policy preset. A risk rule is a triple:
  what it measures, on what basis, and what it does on breach - flatten and
  lock until the next session boundary, or flatten and terminate. The account
  defines its own day as a minute of the UTC day; enforcement is the venue's,
  because a strategy that would have been liquidated must actually be
  liquidated or the forward claim is worth nothing.
- **Client**: an overloaded word, and the glossary's job is to name the
  overload rather than pretend one sense wins everywhere. In mogwai's own
  prose it means the counterparty PROCESS - typically a nautilus host - which
  holds several connections (a data leg and an exec leg) under one account
  and identifies itself to the venue only by its session id. Nautilus uses
  the same word for the adapter OBJECTS inside that host (the
  `MogwaiDataClient` / `MogwaiExecutionClient` pair a host registers), so
  "the adapter's client pair" is one process carrying two nautilus clients.
  And in wire field names (`client_order_id`) it means the submitting side's
  own namespace, as opposed to ids the venue mints. The venue itself never
  perceives a client as more than a session id, an account and its
  connections.
- **Connection**: one WebSocket under an account, bound to one river at one
  speed. Delivery, transport havoc and byte budgets are per connection;
  ownership of orders and money is per account. The word covers WebSockets
  only: an HTTP history poll or a control-plane POST is a wire interaction
  but not a connection, holds no lane and no seat, and survives nothing.
- **Session**: the self-asserted client identity carried on the upgrade as
  `/ws?session=`, minted once per adapter process. Sockets presenting the same
  account and session coexist - that is what lets one client hold two legs
  without evicting itself - while a different or absent session evicts the
  incumbent. Silence is never a claim to be the incumbent, and no
  authentication stands behind any of it.
- **Seat**: an account's riding of one boat, counted per connection - the seat
  is vacated by its last rider. An account holds as many seats as the distinct
  boats its sockets have bound, so one account trades many rivers at once (many
  strategies, one ledger). The one refusal is a second speed of a river the
  account is already riding: one ledger carries one cadence. A freeze clears
  every seat, and when a frozen account returns, what its book holds off the
  river the returning socket joins is retired - resting orders cancelled,
  positions closed at their last mark - because the new session could neither
  see nor close it. A live account binding a second symbol retires nothing;
  that is the supported many-rivers shape, not a return.
- **Eviction**: a socket claiming a seated account id from a different client -
  a different session, or none - closes the incumbent and inherits the account:
  ledger, orders, risk state. The same client's own sockets coexist instead,
  which is what lets one process hold its two legs and trade several symbols
  under one account without evicting itself. The close is
  normal (WS 1000, with a machine-readable evicted reason), not a fault: from
  the venue's side a reconnecting client and a stranger claiming the id are
  indistinguishable, so handing the account over is the only behaviour that
  lets a killed worker come back to its own book. A consumer must not treat it
  as a reason to redial, or it evicts whatever evicted it.
- **Freeze**: the state of an account whose last connection went away. A frozen
  account is not swept, not marked, not funded and not judged against its
  policy until a socket returns - a deliberate departure from a real venue,
  where being away is no defence against liquidation, and a gap any claim over
  the run must state. Bounded by `account_ttl_ms`: an account nobody reclaims
  inside it is collected.
- **Strategy**: the consumer's unit of work - one trading program, driving one
  account over one instrument (single-instrument by settled premise). The venue
  never sees a strategy; it sees an account and its connections, which is why
  everything a strategy needs must be expressible per account or per
  connection.

## Havoc

- **Divergence**: one armed havoc injection, posted on `POST
  /control/divergence` and by convention posted per account on connect,
  constant for that connection. The classification test for any arm is whether
  it changes the water or the view. Generator arms (`VolStorm`, `FlowSurge`,
  `LiquidityDrought` and kin) change the water, so they are part of river
  identity and fork the river at placement rather than mutating shared water.
  Transport arms (`GoDark`, `StallData`, `DelayAcks`, `CommandLatency`)
  corrupt what one account's connections receive, so they ride the passenger.
  Engine arms (`PartialFillNext`, `RejectNextSubmit`, `RejectNextCancel`,
  `DuplicateNextFill`, `DropNextAccountUpdate`) queue one-shot execution
  divergences on the account's own ledger, and windowed account-side arms
  (`FeeSurcharge`) apply to the ledger for their span. `FaultTape` stands
  alone: it is terminal, taking the whole venue down through its fault
  channel.

## The wire

- **ReadyRecord**: one versioned JSON line describing the venue, its only
  stdout output. It names no symbol; attach identity is `addr` plus `run_seed`.
- **RunComplete**: the terminal WebSocket announcement for a planned duration
  completion, followed by a normal close. A socket may carry its own
  `duration_ms`, measured in simulated milliseconds on its boat's clock from
  its own boarding instant, so passengers on one boat complete independently.
