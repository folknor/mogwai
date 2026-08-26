# Accounts

An account is an id plus everything the venue holds under it: one ledger - a
book of balances, positions and order history - its risk state, and its havoc
arms. A run holds one account per id it has seen, and they share nothing.

Nothing has to be opened before trading. Account resolution is total: a socket
that names no account is served under the run's default account, and a socket
that names one gets that account, created on first sight of the id if the run
has not seen it. `POST /accounts` exists for the case the default cannot
express - several consumers on one venue, each funded or policed differently -
and a run with one consumer needs no call at all.

The id is the consumer's rather than minted by the venue, because a stable id
is what makes a returning socket a continuation of the same book. It is
therefore a bearer token: anyone who names an id claims that account, and the
venue cannot tell a reconnect from a stranger presenting the same id. That is
acceptable on a loopback venue serving one orchestrator's workers, and it is
written down here so it is not read as more than it is. There is no
authentication of any kind on any account surface.

An id is 1 to 64 bytes of ASCII letters, digits, dot, underscore, colon or
dash; anything else is refused by name. The `[balances]` table, the two
lifecycle keys and the policy registry are documented in `docs/config.md`;
order entry, history paging and havoc are out of scope here and live in
`docs/cli.md`, `docs/config.md` and `docs/havoc.md`.

## The default account

`account_id` in the run config names the default account and defaults to
`MOGWAI-001`. It selects which ledger a connection presenting no id is served
under; it does not declare the venue's one account, and every socket may name
its own.

The value must have the `ISSUER-NUMBER` shape and boot is refused otherwise.
That is a nautilus rule rather than a wire rule - mogwai's own account id
accepts a bare word - but a nautilus `AccountId` cannot be built from one, so a
venue reporting `MOGWAI` would boot cleanly and be rejected by its consumer.

Who owes an id is a usage question the venue cannot answer for you. On a shared
venue - one `mogwai serve` several consumers dial - every consumer names its
own, because two consumers presenting one id are one trader as far as the venue
can tell. On an ephemeral venue spawned for one run, nobody owes anything, and
that is what the default is for.

The default account is unpoliced and is funded from `[balances]`, which is the
opening balance every account gets when its consumer names none.

## Opening an account: POST /accounts

The body is JSON and unknown fields are refused:

```json
{
  "account_id": "WYRD-820",
  "balances": {"USD": "250000"},
  "policy_preset": "intraday-trail"
}
```

`balances` is the opening balance by currency, and every amount is spelled as a
string. It may be omitted when the selected policy carries `opening_balances`;
an explicit request table wins. A bare JSON number decodes through `f64`, so a wide balance would be
silently rounded and `1e-30` would fund the account with nothing; a numeric
spelling is refused. `policy` carries risk knobs inline instead of naming a
preset, and its thresholds stay number-tolerant, because a policy is also
written in TOML.

A successful open answers `201` with an empty body. The refusals, with the text
the venue actually sends:

- `400 account id is not usable: ...`, naming what is wrong with the id.
- `400 an account must open with at least one funded currency, either in
  balances or in its policy`. An account
  funded in nothing would meet a funds rejection on its first order, which
  reads as depletion; naming it here keeps a configuration mistake apart from a
  trading outcome.
- `400 this account opens with <currency> under a policy stated in
  <policy currency>: ...`. A policed account may hold only its policy currency,
  because equity is computed in that currency alone and the venue has no
  exchange rate. Counting a foreign balance toward the anchor would open the
  account above any equity it can observe and liquidate it on its first mark,
  so the configuration is refused rather than converted at parity. An unpoliced
  account is anchored by nothing and may hold any mix.
- `400` naming an unusable policy field - `trailing_drawdown.amount must be
  positive`, `reset_minute_utc must be a minute of the day, 0 to 1439`, and the
  rest - so a nonsense rule is a refused request rather than an account that
  behaves strangely hours later. A shipped preset is validated the same way an
  inline policy is.
- `400 no account policy is registered or shipped under <name>; shipped names
  are ..., and an operator registers more under [account_policies] in the venue
  config`. A name nobody has is an error rather than a quiet fall to unpoliced,
  because a run that believes it is enforced and is not is the worst outcome
  available.
- `409 this account is already open; an account outlives its connections, so it
  is never re-opened with new terms - name a different account id for a fresh
  ledger`. Re-opening is ambiguous between starting a fresh experiment and
  re-sending a config after a reconnect, and the second reading would wipe a
  live position book.

An account opened this way and never connected to is unattended like any other,
which means it is frozen from the moment it is created and is collectable once
`account_ttl_ms` is set. Opening one does not reserve it forever.

## Naming an account on a socket

A socket names its account with `/ws?account=`. An absent value binds the
default account. An id the run has not seen creates that account there and
then, funded from `[balances]` and unpoliced - which is why an account only
needs `POST /accounts` when its terms differ from the run's.

Two refusals are decided before anything is claimed, so a refused upgrade
displaces nobody:

- funding. `account <id> is not funded in <currency>, which is what <symbol>
  settles in; open the account with a <currency> balance`. The check is asked
  of the ledger the connection will actually get, which under
  `reset_account_on_reconnect` is a fresh one built from `[balances]`.
- cadence. `account <id> is already seated on <symbol> at speed <speed>; a
  ledger carries one cadence`. Two sockets of one account cannot ride two
  speeds of one river, because that would judge one ledger on two clocks. The
  rule holds while any of the account's passengers is riding that river and
  lifts once the last leaves; other rivers are unaffected, and other accounts
  may ride the same river at any speed they like.

An account rides as many rivers as its passengers have boarded. Every socket
under one id acts on that one ledger whatever symbol it bound, so a consumer
trading two instruments under one id is trading one book. Positions are keyed
within it by the run's `oms_type`; see `docs/oms-types.md`.

## What one account may run

Three shapes come up, and only the third is outside what mogwai supports.

**Many strategies, different symbols, one account.** Supported, and it is the
design. This is the many-rivers shape: each strategy boards its own passenger
onto its own river, and all of them settle onto the one ledger the account id
names. Fifty of these under `MOGWAI-001` is server mode working as intended.

**Many strategies, the same symbol, one account.** Permitted, and the venue does
not care. Whether they land on one river or on several - a different seed or a
different generator arm gives a different river key - is immaterial to it. It is
usually an operator mistake rather than a plan, because those strategies share
one ledger and so net against each other, and because a ledger carries one
cadence per river. Neither is a ground for refusal: mogwai serves what is asked
for and leaves the arrangement to the operator.

**One strategy reading several symbols.** Not supported. This is the
cross-symbol thesis - BTC and ETH read together, or an MNQ/MES divergence trade
- where what the strategy trades is the relationship between two instruments
rather than either one alone. A strategy here is single-instrument by settled
premise, and the reason is the water: per-symbol tapes are generated
independently and carry no cross-instrument correlation, so such a strategy
would be trading a relationship that does not exist in the tape. It would not
fail loudly; it would return a number that means nothing.

Nothing in the venue enforces the third case, because the venue never sees a
strategy at all - it sees an account and its passengers, and one strategy
holding two passengers is indistinguishable from two strategies holding one
each. The premise is the consumer's to keep. `mogwai-adapter` keeps the local
half of it by binding one data client to one river and refusing any further
subscription. See the Strategy and Account entries in `reference/glossary.md`.

## Callsigns, coexistence and eviction

`/ws?callsign=` carries the identity a socket presents: 1 to 64 characters of
ASCII letters, digits, dot, dash or underscore. Sockets presenting the same
account and callsign coexist on one ledger. A socket presenting a different
callsign, or none, claims the account: every incumbent socket is closed and the
newcomer inherits the ledger.

Absent means evict, on both sides. Silence is not a claim to be the incumbent,
so a socket naming no callsign displaces whoever is there and is displaced in
turn, which is what every socket did before callsigns existed. Coexisting is
opt-in, and the safe reading is what saying nothing gets you.

The carrier exists because a nautilus host dials `/ws` twice, once for market
data and once for execution, and both legs name one account by construction:
eviction keyed on the id alone would make the second dial disconnect the first.
`mogwai-adapter` mints a callsign per process from the pid and the start
instant and puts it on both objects, so a host configures nothing and a
restarted worker reclaims its ledger from the sockets of the dead one. The
venue reads nothing into the string beyond equality, and like the id it is a
bearer token: knowing the pair joins that ledger rather than displacing it.

An evicted socket is closed normally - WebSocket code 1000, reason `evicted:
another connection claimed account <id> under a different callsign` - and not
as a fault, because nothing failed. A consumer must not treat an eviction as a
reason to redial, or it evicts whatever evicted it. The reason prefix is the
machine-readable part; match on that rather than on the sentence.

Naming no account never evicts. A consumer opening two sockets on two symbols
without naming an id lands both on the default account, and that shape is
supported rather than self-evicting.

## Freeze, return and the TTL

An account whose last socket went away is frozen. While frozen it is not swept,
its positions do not mark, its funding does not accrue and its policy cannot
liquidate it. That is deliberate - mogwai exists to exercise a consumer's live
path, not to run an account nobody is trading - and it means a run spanning a
disconnect has a gap in its risk history, which any claim over that run owes a
sentence about. A real venue offers no such shelter.

An account is created frozen, whether it was minted by a socket that then
attached or opened by `POST /accounts` and left alone.

When a socket returns to a frozen account, three things happen. The freeze
lifts. Whatever the account holds off the river the returning socket bound is
retired - resting orders cancelled, positions closed at their last mark -
because the returning connection can neither see nor close it. And every
surviving order's scan frontier is re-based onto the new boat's clock, since
the departed boat's frontier sits in the new cursor's future; the span while
nobody was reading was never watched and no fill is owed for it, which is the
same statement the freeze makes. A live account boarding a second river retires
nothing: that is the many-rivers shape, not a return.

While a consumer is attached, an order on a symbol none of that account's own
cursors is reading is cancelled rather than left resting. Nothing could ever
fill or expire it, and the consumer is there to be told. The set is the
account's own rides and not every river the venue happens to be running: the
sweep decides an order only for an account seated on the boat that came due, so
another account riding the symbol would leave this one's order undecided
forever.

`account_ttl_ms` (default `0`, meaning never) is how long an unattended account
survives before the venue collects it, in wall milliseconds rather than
simulated ones, because a frozen account has no simulated clock. It is the only
thing that ever removes an account: a frozen ledger has no lifecycle of its
own, so without it a long-lived shared venue accumulates one ledger per id
anybody ever presented. A collected account is gone - not liquidated, not
judged, simply removed - and the next socket presenting that id opens a clean
ledger. Set the TTL longer than the slowest restart any consumer performs.

`reset_account_on_reconnect` (default `false`) decides whether a returning
consumer gets its ledger back or a clean one. False is what makes a reconnect a
continuation. Set it true when a batch reuses ids across independent
experiments. A socket presenting the callsign of one already on the account
never resets, because that would discard the ledger the sitting socket is
trading on. Both keys are reported on the readiness record, so a launcher never
has to infer which way a venue is set.

## Risk policies

A policy is the rules the venue enforces against one account. This is a
risk-policy layer rather than a funded-account feature: a live venue has the
same machinery, and a firm's programme is that engine with stricter numbers.
Enforcement is the venue's own, because a strategy that would have been
liquidated must actually be liquidated or the forward claim is worth nothing.

A rule is a triple - what it measures, on what basis, and what it does on
breach - and two breach actions cover the cases:

- `lock_until_reset` flattens the book and refuses to open until the next
  reset, then resumes with a fresh budget.
- `terminate` flattens the book and ends the account. There is no tomorrow.

The rules, all optional, and an account naming none is unpoliced:

- `trailing_drawdown` - `amount`, a `basis` of `peak_equity` (intraday peak
  including unrealized) or `end_of_day_balance`, an optional `lock_at_equity`
  where the trail stops following, and `on_breach`, defaulting to `terminate`.
- `daily_loss_limit` - `amount` measured from the day's opening equity, and
  `on_breach`, defaulting to `lock_until_reset`.
- `overall_drawdown` - `amount` measured from opening equity, never ratcheting
  and never resetting, with `on_breach` defaulting to `terminate`.
- `max_position` - `quantity`, one scalar applied independently to every symbol
  in the instrument's own size unit. It is refused at order entry rather than
  flattened afterwards: it bounds the largest position the book can reach after
  this order given worst-case fill order of the working book, so ten is ten
  contracts on one symbol and ten coins on the next, and an account trading
  several symbols may hold the cap in each at once.

`reset_minute_utc` (default 1320, which is 22:00 UTC) is the minute of the UTC
day the daily budget resets. The account defines its day, not the instrument.
The reset fires whenever sim time crosses it. A socket cannot bind such a
policy to a footprint that never contains the instant: the upgrade refuses the
pair by name, so a daily limit cannot silently become a run-lifetime limit.

`currency` is required whenever any rule is set, and refused for absence with
`a policy with any rule must name the currency its thresholds are stated in;
equity is computed in that currency alone and the venue has no exchange rate`.
The consequence is that a policed account trades one settlement currency, which
today means futures: an order that would open a second currency is refused at
entry by name rather than mis-valued afterwards.

Policy resolution is total and three-step, the same shape a symbol resolves in.
Inline knobs win; otherwise the name in `policy_preset`, resolved against
`[account_policies]` in the run config first and this build's shipped names
second, with a registered name shadowing a shipped one; otherwise unpoliced. A
name neither has is an error. Registration exists because shipped terms go
stale.

This build ships five names, and they are illustrative rather than any firm's
current terms: `intraday-trail`, `eod-trail`, `daily-limit-only`,
`static-drawdown` and `intraday-trail-sized`. Their numbers are listed under
"Account policies" in `docs/config.md`.

When one equity reading crosses several rules at once, a terminating rule wins
over a lock, so a softer floor earlier in evaluation order cannot mask a hard
one. Rules carrying the same action keep their evaluation order.

Once a rule fires, that breach is what describes the run: a breached account is
not re-evaluated, and only a terminating rule is recorded as the breach. A
`lock_until_reset` breach is remembered as a lock and lifts at the next
crossing of the reset minute. It is acted on once: the flatten and the breach
frame happen on the crossing, and the account is then inert rather than
re-flattened on every sweep for the rest of the period. A rule that would have
fired underneath the lock is evaluated on the first reading after the lift.
A locked account's submits are refused with
`account <id> breached its risk policy and may not open a position`, while
cancels and queries are still served - a locked consumer must be able to see
and tidy its own book, and refusing its queries would make a locked account
indistinguishable from a broken one.

## Reading an account: GET /account

`GET /account?account=<id>` reports whichever ledger it names, defaulting to
the default account, which is the same resolution the socket does. Consumers
pull it once on connect so the bridge's account row exists before the first
order is worked, rather than learning the account from the first fill.

An id nobody has traded under is not an error, and asking about an account no
longer opens one: the answer is the opening balances a ledger under that id
would carry, built and thrown away. A malformed id is a `400` naming what is
wrong with it, and so is any query key other than `account`, since a misspelled
key would otherwise hand back the default account's snapshot under the name of
the account you asked about.

The answer carries three fields: `clock`, `account` and `risk`. `clock` is
always `venue`, and the stamp is deliberate. A ledger spans every river its
account's passengers have boarded, so there is no boat clock to put it on:
stamp from one boat and a push from a later-placed boat is ahead of the pull,
stamp from the newest and it is behind. No choice keeps a cross-clock
monotonicity promise, so the answer says which clock it used and a consumer
orders pulls against pushes by sequence.

`risk` publishes equity, the ratcheted peak, the day's opening equity, whatever
thresholds and remaining budgets the policy defines, the position cap and any
breach that has fired. Every decimal in it is string-spelled, and a numeric
spelling is refused on decode. An unpoliced account still reports its equity
when it holds exactly one currency, since that is the one number an evaluator
wants whether or not anything is enforced.

The audience for the pulled answer is the evaluator: without these numbers a run
that ended flat having spent ninety percent of its budget is indistinguishable
from one that never came close.

## What a balance's locked amount is made of

Each balance row carries `total`, `free` and `locked`. `locked` is everything
this currency is not free to spend - exactly `total - free`, and exactly what
the venue's own funds check subtracts.

Three unrelated things sum into it, and they release on unrelated events, so
the sum alone cannot tell a consumer what would free the money:

- **order holds** - funds reserved against resting orders, released by
  cancelling or filling them;
- **margin** - maintenance collateral posted against open marked positions
  (futures, perpetuals, inverses, and an equity whose margin policy makes it a
  Reg-T margin account), released by closing the position, and moving on its
  own as the mark moves;
- **unsettled** - sale proceeds the account owns but cannot spend until their
  settlement instant, released by the passage of simulated time and by nothing
  the consumer can do.

Every balance row therefore carries a `held` object with those three fields
beside `locked`. They sum to `locked` by construction - the total is formed by
adding the three, rather than computed separately and hoped to agree - and
every one is string-spelled like the rest of the money on the wire.

`locked` keeps its meaning and is still the right answer to "how much can I not
spend". `held` is the answer to "and what would release it".

## A strategy sees its own remaining budget

The same risk block rides on every pushed `AccountState` frame a policed
account's passengers receive, as an optional `risk` field beside `balances`,
`positions` and `margins`. So a strategy can size against its own remaining
drawdown as it trades, which is what a real trader reads off the firm's
dashboard. Before this it existed only on the pulled `GET /account`, which meant
a strategy driving the venue through a nautilus host could not reach it at all.

Two properties to rely on, and one not to.

The field is absent for an unpoliced account. It has no thresholds, so it has no
budget to report, and an all-absent risk block would read as a policed account
with nothing left rather than an account under no policy. Absence is the signal.

The numbers are as stale as the last sweep. `equity` inside the block is the
figure the last risk evaluation used, not one recomputed for the frame it rides
on - the same staleness the account's marks already carry. Recomputing per frame
would price a mark walk into the order path and would publish a budget the
enforcement had not acted on.

What not to rely on: the field is optional on the wire, so a decoder written
before it existed ignores it and behaves exactly as it did. A missing `risk` is
therefore never evidence that a budget was exhausted.

Through the nautilus adapter these arrive in `AccountState.info` under
`mogwai_`-prefixed keys - `mogwai_trailing_remaining`, `mogwai_daily_remaining`,
`mogwai_overall_remaining`, the thresholds, the peak, the day's open, the
position cap, and the breach's rule, action and instant if one has fired. Read
them with `get_str` and parse: they are string-spelled for the same reason they
are on the wire, and `get_f64` would reintroduce the very tolerance the string
spelling exists to prevent.
