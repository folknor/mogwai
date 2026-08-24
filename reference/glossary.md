# mogwai glossary

The working vocabulary, split water-side (the tape and its machinery) from
account-side (the traders and their money). Each entry states what the word
means here and, where the meaning is load-bearing, the one fact that makes it
so.

This document exists so that a conversation about the difficult parts of mogwai
- the rivers, the boats, the passengers and what they owe each other - can be
had without the parties meaning different things. That is the admission test,
and it is narrow on purpose: a word earns an entry when using it plainly would
leave two people believing they agreed. The river, boat and passenger machinery
is the core of it, and a few words grew out of that core because the core
cannot be stated without them - venue, boarding, callsign.

Being load-bearing is not the test. Neither is being undefined. Most of this
project's nouns are internal mechanism that no discussion turns on, and writing
them down here does not make the vocabulary sharper - it buries the words that
do the work among words that never confused anyone. An entry describing how a
mechanism currently behaves is worse than none, because this document states
the end state and the code moves toward it; a description of the present, once
written here, quietly becomes the target.

Only the owner adds an entry.

## The venue and its modes

- **Venue**: one running instance of mogwai, serving the `MOGWAI` venue over
  loopback: one run, many rivers, many accounts. It gates on no symbol and
  admits any account id; both resolutions are total. Whether it runs as its own
  PID or embedded in the consumer's own program is a deployment detail and never
  part of its identity - `mogwai-venue` is a library, and the venue is one
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
  as many boats as its passengers' distinct (river, speed) boardings. A run may
  declare a simulated duration, and so may an individual passenger.

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
  passenger boards and carrying its own `SimClock`, its own broadcast ring and
  its own market-reading memo. Passengers asking for the same river and the
  same speed share one boat; a different quantized speed places a second boat
  on the same water, because speed is not part of river identity - it changes
  delivery cadence and never a generated value, so two boats at two speeds are
  reading one river. One ledger still carries one cadence. A boat is
  an implementation cache with no semantics of its own: the tape is
  deterministic and exogenous, so nothing a consumer can measure reveals
  whether it shares a hull.
- **Boatyard**: the run-owned registry of keyed boats and the tickets that keep
  them alive. A boat winds down when its last passenger leaves.
- **Boarding**: the act, at connect, by which a passenger's resolved config
  selects its water. It is the one moment when identity is decided, and every
  other entry here depends on it. Each passenger boards alone: an account's
  several passengers may each want different water, and the account they belong
  to is what their orders and money land on, never what selects their river.
  The carrier decides nothing: a knob posted to the control plane and one read
  from the venue's config are the same input by the time boarding happens, so
  when or how a knob arrived says nothing about whether it is part of a river's
  identity. And the river key is whatever in that resolved config can mutate
  the water: a key naming a river that already exists boards the passenger onto
  it, sharing the boat already reading it, and a key naming none creates the
  river and places a boat for it. So a passenger boarding later is not joining
  a river's past or altering it - it resolves a key like every other passenger
  and gets the water that key names.
- **Tape**: what a boat publishes - the paced frame stream broadcast to that
  boat's passengers only. No river exists until a boarding or a history poll
  first names it, and it is synthesized then.
- **Warmup**: the uniformly servable simulated history from `data_origin_ns`
  through `run_start_ns`. `warmup_ns` is their distance, and every river owes
  the whole span before it can be served: it is what history queries answer
  from and where a strategy's warmup bars come from, paid by the river's first
  requester inside its own request.
- **Served symbol**: any symbol a request names that resolves to a legal,
  fundable shape. A symbol with a preset gets that preset's shape; one without
  gets the default shape under its own label, memoized per run. A request can
  still be refused, on any of five grounds: an illegal label, a shape that does
  not validate, a settlement currency the account or run holds no balance in, an
  exhausted river cap, or a second cadence on a river the account is already
  riding. What is never a ground is the absence of a preset.

## The instruments

- **Instrument class**: the settlement shape an instrument takes, which is what
  decides how holding it moves the ledger. Six: `spot` (a base/quote pair -
  the base is held as a currency balance), `forex` (a marked leveraged
  base/quote position carrying pip, point and daily swap conventions),
  `equity` (a share - held as a
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
  loopback venue and written down so it is not assumed to be more. An account
  outlives every passenger that speaks for it, and rides as many rivers as its
  passengers have boarded - many strategies, one ledger, one cadence per
  river.
- **Ledger**: one `mogwai-engine` instance, owned by one account and created on
  first sight of that account id. A run holds as many as it has accounts and
  they share nothing: positions, balances, order history and armed divergences
  are all per ledger. Every socket a consumer opens under one account id acts on
  that account's ledger, whatever symbol each bound, so a consumer trading two
  instruments is trading one book. Order entry is WebSocket-only - there is no
  HTTP order carrier. A ledger outlives every passenger that writes to it,
  which is what makes a reconnect a continuation.
- **Passenger**: one connected trader - a single WebSocket under an account,
  boarded onto one boat, holding its own lane, its own byte budgets, its own
  declared duration and its own view of the water. Many passengers may ride
  under one account, and what a passenger owns is the ride, never the money:
  orders and funds always land on the account's ledger, so two passengers of
  one account are one trader's two presences, not two traders. A passenger
  survives nothing - it dies with its socket, and what outlives it is the
  Account. An HTTP history poll or a control-plane POST is a wire interaction,
  not a passenger: it boards nothing, holds no lane, and survives nothing.
  Passengers of different accounts owe each other non-interference (which the
  tape's exogeneity gives - order flow never feeds back into the water) and
  invisibility (which attribution and per-account ledgers give: every order is
  claimed for its account - a venue-originated liquidation by the ledger that
  produced it - so every frame is delivered to the account it concerns).
- **Account policy**: the rules an account is enforced under - opening balance
  plus risk rules, named inline or as a policy preset. A risk rule is a triple:
  what it measures, on what basis, and what it does on breach - flatten and
  lock until the next session boundary, or flatten and terminate. The account
  defines its own day as a minute of the UTC day; enforcement is the venue's,
  because a strategy that would have been liquidated must actually be
  liquidated or the forward claim is worth nothing.
- **Consumer**: the program or system driving the venue - broadarrow is the
  known one. It is not a single process: it may be one, several that share
  nothing but the wire, or the very process the venue is embedded in. So the
  venue never perceives a consumer at all. What
  it perceives is a callsign, an account and that account's passengers, and
  the word for the party on one socket is therefore Callsign, never Consumer.
  `client` is not used for anything this project owns. It survives in two
  inherited spellings and nowhere else: nautilus's adapter objects (the
  `MogwaiDataClient` / `MogwaiExecutionClient` pair a consumer registers), and
  the wire field `client_order_id`, which names the submitting side's own id
  namespace as opposed to ids the venue mints.
- **Callsign**: the self-asserted identity carried on the upgrade as
  `/ws?callsign=`. It is announced by the party itself, conventionally
  honoured, and nothing stands behind it. It is also the
  only identity the venue has, so every rule about who may coexist and who
  evicts whom is stated over it rather than over the consumer, which the venue
  cannot perceive. Sockets presenting the same account and callsign coexist -
  that is what lets one leg pair live under one account without evicting itself
  - while a different or absent callsign evicts the incumbent. Silence is never
  a claim to be the incumbent. The adapter mints one per process, so a consumer
  spread across several processes is several callsigns unless it supplies one
  deliberately: sharing a book across processes is a consumer's own act, not
  something the venue infers.
- **Eviction**: a socket claiming a ridden account id under a different
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
- **Freeze**: the state of an account whose last passenger left. A frozen
  account is not swept, not marked, not funded and not judged against its
  policy until a passenger returns - a deliberate departure from a real venue,
  where being away is no defence against liquidation, and a gap any claim over
  the run must state. When a frozen account returns, what its book holds off
  the river the returning passenger boards is retired - resting orders
  cancelled, positions closed at their last mark - because the returning
  passenger could neither see nor close it. A live account boarding a second
  river retires nothing; that is the supported many-rivers shape, not a
  return. Bounded by `account_ttl_ms`: an account nobody reclaims inside it is
  collected.
- **Strategy**: the consumer's unit of work - one trading program, driving one
  account over one instrument. Single-instrument by settled premise, and the
  constraint is on the strategy and never on the account: an account carries as
  many single-instrument strategies as its consumer launches, over as many
  symbols. What has no forward test here is the other shape - one strategy
  reading two instruments together, a BTC/ETH or MNQ/MES relationship trade -
  because independent per-symbol tapes carry no cross-instrument correlation for
  such a strategy to trade. The venue never sees a strategy; it sees an account
  and its passengers, which is why everything a strategy needs must be
  expressible per account or per passenger.

## Havoc

- **Divergence**: one armed havoc injection. It reaches the venue either from
  config or on `POST /control/divergence`, and which carrier it came by decides
  nothing: a divergence is resolved with the rest of a passenger's config at
  boarding, and is constant for that passenger. The classification
  test for any arm is whether it changes the water or the view. Generator arms
  (`VolStorm`, `FlowSurge`, `LiquidityDrought` and kin) change the water, so
  they are part of river identity: a passenger whose resolved config carries
  one boards a different river than a passenger without it, and that is all
  "forking the river" means. Nothing mutates water someone is already reading.
  Transport arms (`GoDark`, `StallData`, `DelayAcks`, `CommandLatency`)
  corrupt what an account's passengers receive - the view, never the water -
  so they are armed per account and blur each of its passengers alike.
  Engine arms (`PartialFillNext`, `RejectNextSubmit`, `RejectNextCancel`,
  `DuplicateNextFill`, `DropNextAccountUpdate`) queue one-shot execution
  divergences on the account's own ledger, and windowed account-side arms
  (`FeeSurcharge`) apply to the ledger for their span. `FaultTape` stands
  alone: it is terminal, taking the whole venue down through its fault
  channel.
