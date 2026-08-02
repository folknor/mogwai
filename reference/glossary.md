# mogwai glossary

The vocabulary for the things that connect to this venue and the things it
serves them. Written because "user", "strategy", "client", "subscriber",
"account", "session" and "tape identity" are used across the code, the
`reference/` documents and `notes/` without settled meanings, and at least one
of them turns out to have no referent in this codebase at all.

Every definition below is grounded in what the code does. Where the code and a
document disagree, where one word carries two meanings, or where a word has no
object, this file RECORDS the discrepancy and leaves it unresolved. It proposes
no rename and picks no winner: a term that three documents use three ways is
recorded as a term that three documents use three ways. The open discrepancies
are collected at the end, and each is cited from the term it belongs to.

`reference/` is durable and citable from source, so this file is binding as a
description. It is not binding as a design: nothing here decides any open scope
question, and where a transient planning document disagrees with this file about
what the code does, this file wins and that document is describing an intent.

## The identity chain, as the code actually builds it

Reading outward from the venue:

```
process (one mogwai-server, one mogwai.toml)
  |- instrument set          per process   [[instrument]] -> InstrumentProfiles
  |- clock, speed, origin    per process   SimClock, data_origin_ns
  |- tape registry           per process   TapeKey -> one OS thread
  |- account registry        per process   AccountId -> AccountSlot
       |- ledger + engine    per account   Engine
       |- armed divergences  per account   atomics on the slot
       |- fill sweeper       per account   one task, only when gated
       |- sessions           per account   one per live /ws socket
            |- outbound lanes  per session HELD + PRIORITY, byte/frame budgets
            |- subscriptions   per session HashMap<symbol, Subscription>
                 |- fanout task            one tokio task per subscription
                 |- tape lease             attaches to a shared tape
```

Nothing in that chain is a strategy, and nothing in it is a user. Both terms
enter from outside; see their entries.

## Terms

### account

An `AccountId` (`mogwai-protocol`) plus the `AccountSlot` it keys in
`mogwai-server`'s `AccountRegistry`. `AccountId` is a string of ASCII
alphanumerics plus `. _ : -`, non-empty, at most `MAX_ACCOUNT_ID_LEN` bytes,
parsed at every stateful boundary.

The slot owns: one `mogwai-engine` `Engine` (the ledger, the order book of
record, the accepted-id map, the closed-order and fill retention), the armed
server-owned havoc windows (`delay_ms`, `dark_until_ns`, `stall_until_ns` and
the six `CommandLatency` fields), the session count, the registry of every
bound session's `ExecLanes`, and the fill sweeper when penetration gating is on.

Accounts are created implicitly on first use of a well-formed unknown id, up to
`max_accounts`; destroyed explicitly by `DELETE /accounts/<id>`; and reaped when
session-less and idle past `account_idle_timeout_ms` of WALL time. Re-creating a
destroyed id yields a new `generation` and an empty ledger.

An account is NOT a credential: identity is a transport attribute (the
`x-mogwai-account` header, or the `account` query parameter on the `/ws`
upgrade), any id may act as any account, and `DELETE /accounts/<id>` takes the
id from the path with no header at all.

Not everything the operator might call account-scoped is: `[balances]` is a
process-wide TEMPLATE that every auto-created account is funded from
identically, and `reference/config.md` says so in those words. See D11, D12.

### admission

The venue's request-handling layer, before any engine state is touched:
`mogwai-server`'s `admission.rs` reserves the worst-case output bytes of a
command (from `mogwai_protocol::sizing`) under the same engine lock that would
process it, and refuses the command if the reservation does not fit.

`EventKind::Admission` is the resulting wire category:
`AdmissionRejected`, `ProtocolError` and `SubscriptionIssues`. Admission frames
ride the session's PRIORITY lane and are exempt from `DelayAcks`, because
`DelayAcks` holds ENGINE OUTPUT and admission truth is never engine output.

`POST /orders` constructs its `ExecLanes` fresh per request and drops them with
it, so the HTTP order carrier has no backlog to protect and can never refuse a
command on admission grounds. See D17.

### client

Three unrelated referents in this repo:

1. **The peer** on the other end of a mogwai socket or HTTP request. This is the
   sense in `ClientMessage`, `client_order_id`, `MAX_CLIENT_ID_LEN`,
   `ClientHavoc` and comments such as "one client cannot". mogwai assigns it no
   identity of its own: the only identity a peer carries is its account.
2. **The nautilus client objects** in `mogwai-adapter`: `MogwaiDataClient` and
   `MogwaiExecutionClient`, built by `MogwaiDataClientFactory` /
   `MogwaiExecutionClientFactory`. These are the "client half" of the havoc
   model (`HavocFilter`, `ConnHavoc`).
3. **The nautilus `ClientId`**, the registration name a factory's `create` is
   handed (`"MOGWAI-TEST"` in the adapter tests). It never crosses the wire and
   mogwai-server never sees it.

One broadarrow worker constructs TWO objects of sense 2 (a data client and an
exec client), each of which opens its own `/ws` socket by default, so one
consumer is two connections to mogwai. See D5.

### connection

One accepted `/ws` socket. Synonymous with **session** in every place the code
uses either word: `ws.rs` says "one task per connection", `accounts.rs` calls
the same object a session, and the budgets are documented per-connection in
`reference/config.md` while `sweeper.rs` and `reference/architecture.md`
describe delivery to the same objects as per session. No code distinguishes
them. See D3.

Per-connection quantities: `exec_held_budget_bytes`, `admission_lane_frames`,
`admission_promise_tickets`, `pending_command_acts`,
`max_subscriptions_per_connection`. The consumer does not choose any of them;
the operator chooses them for every consumer at once.

An HTTP request is not a connection in this sense. It takes no `SessionLease`,
holds no lanes past its own lifetime, and does not appear in the `sessions`
count `GET /accounts` reports.

### divergence

`mogwai_protocol::control::Divergence`, the nine-variant catalog armed over
`POST /control/divergence` with an `x-mogwai-account` header. Split by owner:

- **Engine-owned, single-shot, queued for a trigger the engine fires**:
  `PartialFillNext`, `RejectNextSubmit`, `DuplicateNextFill`,
  `DropNextAccountUpdate`.
- **Server-owned temporal windows**, atomics on the arming account's slot,
  applied in the session's outbound path: `DelayAcks`, `StallData`, `GoDark`,
  `CommandLatency`, plus `ClearDivergences` which lifts them.
- **Immediate-action**: `CancelOpenOrderSilently` acts on the book when posted.

A divergence is strictly narrower than **havoc**: `MarketRegime`, `ClientHavoc`
and `ConnHavoc` are havoc surfaces that never travel this plane. And a
`SubscriptionIssue::FeedLagged` is deliberately NOT a divergence: nobody armed
it, so it is a venue fault. See D4, D14.

### engine

`mogwai_engine::Engine`: the venue-agnostic exchange core. Synchronous and side
effect free (`process` takes a `ClientMessage` and returns the `ServerMessage`s
to send), owning order state, the ledger, the instrument table used for submit
validation, the terminal-order and fill truth stores the reconciliation queries
answer from, and the armed engine-side divergence queue. One `Engine` per
`AccountSlot`, behind that slot's async mutex.

It is not the data path (the generator and the tape threads are outside it) and
not the transport (sockets, timers and the clock are the server's).
"Engine" also appears in this repo meaning nautilus's `DataEngine` /
`ExecutionEngine` on the consumer side; those are the adapter's counterparties,
not this one. See D16.

### instrument

An `InstrumentDef` on the wire: symbol, base, quote, price and size precision,
price and size increment. Served by `GET /instruments`, authoritative for submit
validation, and the source of the precision the adapter converts `Decimal` ticks
to nautilus `Price` / `Quantity` at.

The instrument SET is per process: the `[[instrument]]` array in `mogwai.toml`,
or the built-in BTCUSDT default. There is no scope smaller than the process at
which it can differ.

On the wire an instrument is addressed by its **symbol** alone (`Symbol` is a
`String`); orders carry a symbol and nothing else. The nautilus `InstrumentId`
(`SYMBOL.MOGWAI`) is constructed adapter-side in `convert.rs` and never appears
in `mogwai-protocol`. See D15.

### profile

At least five unrelated referents, three of them in the venue itself:

- **`InstrumentProfile`** (`mogwai-server/src/source.rs`): the per-symbol bundle
  of an `InstrumentDef`, a `GeneratorScalars` and a `SessionProfile`.
  `InstrumentProfiles` holds them by symbol and is what both data carriers, the
  price stamp and the sweeper look a symbol up in.
- **`SessionProfile`** (`mogwai-data`): 24 hourly arrival shares, 24 hourly
  volatility multipliers, 7 day-of-week weights. A trading-session envelope, not
  a connection session.
- **`TransportProfile`** (`mogwai-protocol`): `WsStreaming` / `HttpOrders` /
  `HttpPolling`, the adapter's transport archetype.
- **`reference/config.md`'s "instrument profile"**: the `[[instrument]]` TOML
  table, and "the built-in BTCUSDT profile" for the default.
- **A named, pre-registered preset** with per-knob overrides. No such thing
  exists in the code; the sense is in circulation because it is what planning
  work has asked for.

Outside the venue the word is also a `.review.toml` model tier and a brokkr gate
profile. See D7.

### session

In `mogwai-server`: one live `/ws` socket bound to one account for its lifetime.
Materialized as a `SessionLease`, an RAII guard that increments the slot's
`sessions` counter and registers the socket's `ExecLanes` in the slot's lane
registry, deregistering both on drop. `next_session_id` numbers them within a
slot; the number is internal and never reaches the wire.

Sessions are what makes an account unreapable while a driver is live, and they
are the DELIVERY scope for account-owned execution: the fill sweeper books fills
into venue truth account-wide and then delivers to whatever sessions happen to
be attached.

`mogwai-data` uses "session" for something entirely different: a TRADING session
(`SessionProfile`, `SessionModulator`, `session.rs`, and the
`SessionEdgeSpike` regime), the hour-of-day and day-of-week envelope. The two
senses share no code and no concept. See D2, D3.

### strategy

Has NO referent in this codebase. mogwai has accounts, sessions, subscriptions
and tapes, and nothing that corresponds to a strategy.

The word is nautilus's and broadarrow's. `StrategyId` is a nautilus identifier;
it appears in mogwai only inside `mogwai-adapter`, where an order's
`strategy_id` is carried through the adapter's own reconciliation mirror so the
events it emits back into nautilus are well-formed. It never crosses
`mogwai-protocol` and mogwai-server never sees it.

Broadarrow's topology is: one worker owns one venue account and hosts N
`PinersStrategy` under one `LiveNode` at `OmsType::Hedging`. So on the mogwai
side, N strategies collapse into one account and (by default) two sockets. There
is no wire field that would let mogwai tell them apart. See D1.

### subscription / subscriber

A **subscription** is one symbol's live feed on one connection. The server keys
them `HashMap<symbol, Subscription>` per socket, so the identity is
`(connection, symbol)`; a `Subscription` holds a cancel flag, its wakeup
`Notify`, its fanout task handle and its `last_sent_ts`.

On the wire a subscription is requested as a `SubscriptionRequest`: a
client-chosen `generation` (which the client must make strictly increasing per
symbol per connection, enforced with `StaleGeneration`), the symbol, an optional
`start_ts` and an optional `MarketRegime`. The server keeps a
connection-lifetime high-water generation per symbol, so an `Unsubscribe` cannot
let a generation be reused. Successful subscriptions get no acknowledgment
frame.

**Subscriber** is used interchangeably with subscription throughout `tape.rs`,
`ws.rs` and the `SubscriptionIssue` docs. It means the same object viewed from
the tape: the holder of one `broadcast::Receiver` and one `TapeLease`. There is
no subscriber identity distinct from the subscription's `(symbol, generation)`
on its connection. See D10.

The `MarketRegime` on a subscription entry is the ONE data-shaping knob a
consumer picks for itself.

### tape

Two senses, both current:

1. **A shared synthesized stream object** in `mogwai-server/src/tape.rs`: one OS
   thread producing pre-serialized frames, broadcast to attached subscribers
   through a bounded ring of `fanout_depth`, refcounted by `TapeLease` and
   reaped at zero, capped process-wide by `max_concurrent_tapes`.
2. **The synthesized tick stream in general**, as in "walks the CLEAN tape", "an
   off-tape window", "the tape origin", "a trades-only tape". This sense covers
   sources that are not sense-1 objects at all: a subscription's private
   backfill, a `GET /trades` history source, and the fill sweeper's penetration
   walk each build their own `GeneratedSource`. See D8.

**Tape origin** is `data_origin_ns`, derived once at boot as
`sim_now_at_boot - backfill_horizon_ns`, the earliest instant any source can
serve, shared by every symbol.

### tape identity

`reference/architecture.md`'s phrase for `TapeKey`: the triple
`(symbol, data_origin_ns, regime)` under which two subscriptions produce
byte-identical streams and therefore share one tape. The regime is part of the
IDENTITY rather than a per-subscriber overlay because the generator consumes it
inside the walk.

Consequences the code relies on: every clean subscriber on a symbol shares one
tape and therefore one thread; an armed regime costs a tape and so cannot
perturb anyone else; two subscribers arming bit-identical regimes share, which
is correct because each would have computed identical bytes privately.

There is no identifier named "tape identity" in the source; it is prose for
`TapeKey`. See D9.

### user

Has NO referent in the code. `mogwai-server`, `mogwai-engine`,
`mogwai-protocol` and `mogwai-data` contain no user concept, no user id, and no
authentication of any kind. The `reference/` documents say **operator** for the
human who writes `mogwai.toml` and runs the binary, and the surviving
occurrences of "user" are "user-facing" and "user config". Transient planning
documents use "user" for that same human. See D18.

### venue

Three senses:

1. **mogwai itself**, the fake broker/exchange process. This is the sense in
   "venue truth", "venue-assigned", "venue fault", "the venue refused".
2. **The nautilus `Venue`** `"MOGWAI"`, single-sourced as `MOGWAI_VENUE_STR` in
   `mogwai-adapter`, reported by both factories and used to build every
   `InstrumentId`.
3. **A broadarrow routing key**, the `"MOGWAI"` string that selects
   `MogwaiAdapter` and a `[venues.MOGWAI]` account-file entry.

**Venue truth** is a load-bearing compound: the content of a `QueryOrders` /
`QueryFills` reply and of `GET /account` is always a truthful read of the
venue's own state. Havoc may delay or drop its DELIVERY; nothing may alter what
it says.

**Venue fault** is its complement and is machine-readable:
`SubscriptionIssue::is_venue_fault` is true only for `FeedLagged`. It means
mogwai FAILING rather than mogwai misbehaving on purpose, and a run that saw one
should be discarded. See D13, D14.

## Recorded discrepancies

Observed facts, deliberately unresolved. Each is a candidate work item; none is
a recommendation.

**D1. "strategy" has no object here.** Planning work has repeatedly asked for a
knob "set per strategy". mogwai has no strategy identity on the wire or in any crate outside
`mogwai-adapter`'s nautilus-facing internals. Broadarrow maps N strategies onto
one account by a convention written down nowhere in this repo, so mogwai cannot
distinguish them even in principle today.

**D2. "session" means two unrelated things.** A `/ws` session (a socket bound to
an account) and a trading session (`SessionProfile` / `SessionModulator` /
`SessionEdgeSpike`, the hour-of-day and day-of-week envelope). Both are current,
both are named `session` in code, and `SessionEdgeSpike` is a market-data havoc
named after the second while riding a subscription on the first.

**D3. "session" and "connection" are used interchangeably, and the budgets are
documented in the other one's word.** The same `/ws` socket is a session in
`accounts.rs`, `sweeper.rs` and `reference/architecture.md`'s delivery
discussion, and a connection in `ws.rs`, `reference/config.md` and the budget
constants. No code distinguishes them. Consequences that fall out of the
identity: an `HttpPolling` or `HttpOrders` consumer owns no session at all, so
it reports `sessions: 0` in `GET /accounts`, gains no reaping immunity, and
must keep its account alive by polling `GET /account`.

**D4. The havoc document contradicts itself on divergence scope.**
`reference/havoc.md` says the `/control/divergence` plane "arms global,
connection-scoped state" in the `MarketRegime` section, calls the server-owned
windows "the temporal, connection-scoped windows" in the catalog preamble, and
repeats "the connection-scoped framing of these divergences" in the transport
section - while its own server-owned section, `reference/architecture.md` and
`accounts.rs` all say the windows are per-ACCOUNT atomics applying to every
session bound to that account. The per-account reading is what the code does.

**D5. "client" carries three meanings and the count of them matters.** The wire
peer, the nautilus adapter client objects, and the nautilus `ClientId`. One
broadarrow worker is one account but two adapter clients and therefore two
`/ws` sockets, so "per client" is ambiguous between one consumer and two
connections.

**D6. a market-data socket must still name an account.** mogwai requires an
account on `/ws` even for a socket that carries no orders and touches no ledger.
`MogwaiDataClientConfig` therefore carries an `account_id` and `ws_url` appends
it as `?account=`.

Both configs default that field to the `UNSET_ACCOUNT_ID` placeholder
(`MOGWAI-UNSET`) and `validate_account_id` REFUSES a config that still carries
it, so an omitted account fails loudly. It previously defaulted to `MOGWAI-001`,
which made an omitted `account_id` indistinguishable from a deliberate one: a
data socket would bind a DIFFERENT account slot than its own exec socket,
auto-creating `MOGWAI-001`, counting a session against it and charging it
against `max_accounts`. That defect is closed on both sides of the wire.

**D7. "profile" has five referents**, three of them live in the venue
(`InstrumentProfile`, `SessionProfile`, `TransportProfile`), one is the config
file's name for an `[[instrument]]` table, and one - the named preset with
per-knob overrides - exists only in `notes/`.

**D8. "tape" is both a shared object and a generic noun.** `tape.rs`'s
refcounted, thread-backed, broadcast tape, versus "the tape" meaning any
realization of the synthesized walk. The sweeper's "clean tape" walk and a
subscription's private backfill are the second sense and never touch the first.

**D9. "tape identity" has no code identifier.** It is
`reference/architecture.md`'s name for `TapeKey`. Recorded as an alias, not a distinct concept.

**D10. "subscriber" and "subscription" are not distinguished.** They name the
same object from two ends. Nothing in the code assigns a subscriber an identity
separate from `(symbol, generation)` on its connection, which means a
per-subscriber overlay has nowhere to attach - the exact reason regime had to
enter `TapeKey`.

**D11. "account" carries four jobs at once**: the ledger key, the tenant key a
socket is bound to, the blast radius of armed divergences, and the nautilus
`AccountId` the exec client presents. Worth separating, because an account is
first a LEDGER, and two strategies sharing a ledger is a different thing from
two sharing a data profile.

**D12. Account scope is not uniform.** Divergences are per-account; `[balances]`
is explicitly not, and `reference/config.md` states "funding is not
per-account". So "per-account configuration" is true of one surface and false of
the other.

**D13. "venue" is the product, the nautilus `Venue`, and a broadarrow routing
key.** Usually disambiguated by context, but "the venue" in a sentence about
adapter wiring can be any of the three.

**D14. "divergence" and "havoc" are not synonyms and are sometimes used as
though they were.** `Divergence` is the nine-variant control-plane catalog;
`HavocSpec` has four surfaces of which only one is a `Vec<Divergence>`. A
`MarketRegime` is havoc and not a divergence; a `FeedLagged` venue fault is
neither.

**D15. The wire has symbols, not instrument ids.** `Symbol` is a bare `String`,
orders carry only a symbol, and the nautilus `InstrumentId` exists only
adapter-side. Anything that wants to address an instrument more finely than by
symbol - a per-consumer instrument profile, for one - has no field to travel in.

**D16. "engine" is mogwai's exchange core and also nautilus's engines.**
`mogwai_engine::Engine` versus `DataEngine` / `ExecutionEngine`, both discussed
in `reference/architecture.md`.

**D17. "per-connection" admission is unreachable on the HTTP carriers.**
`POST /orders` builds its lanes per request, so it can never emit an
`AdmissionRejected` for a budget it does not have; it is covered by
`global_pending_command_acts` alone. The same asymmetry means the temporal
windows cannot reach an HTTP carrier at all, which the adapter configs refuse
outright rather than silently ignore.

**D18. "user" is the human operator and nothing else.** The code has no user
concept and no authentication. `reference/config.md` and `reference/cli.md` say
operator; `notes/` says user for the same person. No document uses "user" to
mean a consumer of the venue, which is the reading the glossary request
anticipated.
