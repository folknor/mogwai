# mogwai glossary

The working vocabulary, split water-side (the tape and its machinery) from
account-side (the traders and their money). Each entry states what the word
means here and, where the meaning is load-bearing, the one fact that makes it
so.

## The venue and its modes

- **Venue**: one running instance of mogwai, serving the `MOGWAI` venue over
  loopback: one run, many rivers, many accounts. It gates on no symbol and
  admits any account id; both resolutions are total. Whether it runs as its own
  PID or embedded in the consumer's own program is a deployment detail and never
  part of its identity - `mogwai-server` is a library, and the venue is one
  thing either way.
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
  seed, generator-level havoc - and never the delivery speed. That identity is
  resolved at boarding, and a river is created when a boarding passenger's key
  names none that already exists; rivers never serialize on each other's
  checkpoint chain.
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
  so nothing a consumer can measure reveals whether it shares a hull.
- **Boatyard**: the run-owned registry of keyed boats and the tickets that keep
  them alive. A boat winds down when its last passenger leaves.
- **Boarding**: the act, at connect, by which one connection's resolved config
  selects its water. It is the one moment when identity is decided, and every
  other entry here depends on it. It is per connection rather than per
  passenger, because an account's several connections may each want different
  water: they
  board separately, and the passenger they belong to is what their orders and
  money land on, never what selects their river. Two things follow, and both are
  easy to get wrong without them written down. The carrier decides nothing: a
  knob posted to the control plane and one read from the venue's config are the
  same input by the time boarding happens, so when or how a knob arrived says
  nothing about whether it is part of a river's identity. And the river key is
  whatever in that resolved config can mutate the water: a key naming a river
  that already exists boards the connection onto it, sharing the boat already
  reading it, and a key naming none creates the river and places a boat for it.
  So a connection boarding later is not joining a river's past or altering it -
  it resolves a key like every other connection and gets the water that key
  names.
- **Tape**: what a boat publishes - the paced frame stream broadcast to that
  boat's passengers only. Materialization cost is paid at two different
  moments: the boot river's warmup is synthesized before the venue writes its
  readiness line, so its cost lands inside boot, while every other river does
  not exist until a socket bind or history poll first names it and is
  synthesized then - so the first requester of a non-boot symbol pays that
  river's warmup latency inside its own request.
- **Boot symbol / boot river**: the shape the run places a boat on before it
  writes its readiness line - the run boards nothing, because boarding is a
  passenger's act and a run takes no seat - and the river a request that names
  no symbol binds. It is the only river warmed eagerly and the only boat that never winds
  down; every other river is boatless until someone boards it.
- **Warmup**: the uniformly servable simulated history from `data_origin_ns`
  through `run_start_ns`. `warmup_ns` is their distance, and every river owes
  the whole span before it can be served: it is what history queries answer
  from and where a strategy's warmup bars come from. When it is paid is the
  Tape entry's split - the boot river before readiness, every other river on
  first read.
- **Served symbol**: any symbol a request names that resolves to a legal,
  fundable shape. A symbol with a preset gets that preset's shape; one without
  gets the default shape under its own label, memoized per run. A request can
  still be refused, on any of five grounds: an illegal label, a shape that does
  not validate, a settlement currency the account or run holds no balance in, an
  exhausted river cap, or a second cadence on a river the account is already
  riding. What is never a ground is the absence of a preset.

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
  distinct from the instrument's margin parameter, which stays venue-side.
- **Variation margin**: the daily settlement transfer. At the settlement
  instant the accumulated difference between the settlement price and the
  position's VWAP moves in actual cash, and the VWAP resets to that price.
- **Session calendar**: the weekly open windows in exchange-local time. A
  scheduled close is configuration and the market is genuinely shut inside it,
  as distinct from `ReopenGap`, which is unscheduled havoc.

## The accounts

- **Account**: an id plus everything the venue holds under it - its ledger, its
  risk state, its havoc arms. Created on first sight of the id and resolved
  totally: knobs the consumer named win, else a policy preset matching a
  requested name, else the default policy. The id is the consumer's, not minted,
  because a stable id is what makes a returning socket a continuation - and it
  is a bearer token: anyone who names it claims it, which is acceptable on a
  loopback venue and written down so it is not assumed to be more.
- **Ledger**: one `mogwai-engine` instance, owned by one account and created on
  first sight of that account id. A run holds as many as it has accounts and
  they share nothing: positions, balances, order history and armed divergences
  are all per ledger. Every socket a consumer opens under one account id acts on
  that account's ledger, whatever symbol each bound, so a consumer trading two
  instruments is trading one book. Order entry is WebSocket-only - there is no
  HTTP order carrier. A ledger outlives the connection that named it, which is
  what makes a reconnect a continuation.
- **Passenger**: the venue-side object for one account riding the run: the
  account id, its engine, its risk ledger, its freeze stamp and its seats. One
  per account, not per connection, and it outlives every connection that speaks
  for it - which is the whole reason it exists, since something has to hold the
  book across a reconnect. What is per connection belongs to Connection and
  Seat: a connection boards, holds a lane and a declared duration, and rides one
  seat; a passenger holds as many seats as its connections have boarded boats.
  A count of riders on a boat is therefore a count of connections and never of
  passengers. Passengers on a river owe each other
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
- **Consumer**: the program or system driving the venue - broadarrow is the
  known one. It is not a single process, and defining it as one is the mistake
  this entry exists to prevent: it may
  be one process, several that share nothing but the wire, or the very process
  the venue is embedded in. So the venue never perceives a consumer at all. What
  it perceives is a callsign, an account and that account's connections, and
  the word for the party on one socket is therefore Callsign, never Consumer.
  `client` is not used for anything this project owns. It survives in two
  inherited spellings and nowhere else: nautilus's adapter objects (the
  `MogwaiDataClient` / `MogwaiExecutionClient` pair a consumer registers), and
  the wire field `client_order_id`, which names the submitting side's own id
  namespace as opposed to ids the venue mints.
- **Connection**: one WebSocket under an account, bound to one river at one
  speed. Delivery, transport havoc and byte budgets are per connection;
  ownership of orders and money is per account. The word covers WebSockets
  only: an HTTP history poll or a control-plane POST is a wire interaction
  but not a connection, holds no lane and no seat, and survives nothing.
- **Callsign**: the self-asserted identity carried on the upgrade as
  `/ws?callsign=`. It is announced by the party itself, conventionally honoured,
  and nothing stands behind it - which is what the word is for. It is also the
  only identity the venue has, so every rule about who may coexist and who
  evicts whom is stated over it rather than over the consumer, which the venue
  cannot perceive. Sockets presenting the same account and callsign coexist -
  that is what lets one leg pair live under one account without evicting itself
  - while a different or absent callsign evicts the incumbent. Silence is never
  a claim to be the incumbent. The adapter mints one per process, so a consumer
  spread across several processes is several callsigns unless it supplies one
  deliberately: sharing a book across processes is a consumer's own act, not
  something the venue infers. The word `session` is never used for this, because
  it belongs to the trading day - see Session calendar.
- **Seat**: an account's riding of one boat, counted per connection - the seat
  is vacated by its last rider. An account holds as many seats as the distinct
  boats its sockets have bound, so one account trades many rivers at once (many
  strategies, one ledger). The one refusal is a second speed of a river the
  account is already riding: one ledger carries one cadence. A freeze clears
  every seat, and when a frozen account returns, what its book holds off the
  river the returning socket joins is retired - resting orders cancelled,
  positions closed at their last mark - because the returning connection could
  neither see nor close it. A live account binding a second symbol retires nothing;
  that is the supported many-rivers shape, not a return.
- **Eviction**: a socket claiming a seated account id under a different
  callsign, or none, closes the incumbent and inherits the account: ledger,
  orders, risk state. Sockets sharing a callsign coexist instead, which is what
  lets one leg pair trade several symbols under one account without evicting
  itself. The callsign is the discriminator here and the consumer is not,
  because the venue perceives only the former: two processes of one consumer
  that mint their own callsigns will evict each other, and that is the
  consumer's to prevent by sharing one, not the venue's to infer. The close is
  normal (WS 1000, with a machine-readable evicted reason), not a fault: from
  the venue's side a returning callsign and a stranger claiming the id are
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

- **Divergence**: one armed havoc injection. It reaches the venue either from
  config or on `POST /control/divergence`, and which carrier it came by decides
  nothing: a divergence is resolved with the rest of a passenger's config at
  boarding, and is constant for that connection. Reading the post as a runtime
  mutation of a venue already serving is the standing misreading of this entry,
  and it is what the Boarding entry exists to foreclose. The classification
  test for any arm is whether it changes the water or the view. Generator arms
  (`VolStorm`, `FlowSurge`, `LiquidityDrought` and kin) change the water, so
  they are part of river identity: a passenger whose resolved config carries
  one boards a different river than a passenger without it, and that is all
  "forking the river" means. Nothing mutates water someone is already reading.
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
