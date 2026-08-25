# Netting and hedging: the venue serves both

mogwai's own run config selects an order-management style with `oms_type`,
either `netting` (the default) or `hedging`. This is a **run-level** choice,
not an instrument one - it applies to the whole venue for the run, the same
way `seed` does, and every account the venue opens inherits it. (`speed` is not
a parallel: it is a default a `/ws` upgrade may override, and two speeds of one
river are two boats.)
One account is one ledger: a consumer trading several symbols books every fill on
all of them into its own account, and the style below decides only how positions
are keyed within it. Two accounts on one venue never share a position book.

- **Netting** collapses every fill on an instrument into one position per
  symbol: an opposing fill reduces or reverses that one position, and a
  consumer-supplied position id is echoed back on the wire but is not used to
  key anything.
- **Hedging** keeps opposing fills as separate positions, keyed by
  `position_id`. An order submitted with no id opens a fresh venue-assigned
  one; a fill reports the id the venue actually booked it against, which
  under hedging may not be the id the consumer sent.

Every price and quantity on the wire is a JSON string, always, in both directions:
`"quantity":"2"`, `"price":"100.25"`, never `"price":100.25`. A numeric
spelling is refused with a decode error rather than accepted, because a JSON
number goes through `f64` and a peer sending `12345678901234567890.123` would
otherwise get `12345678901234567000` booked as its price, silently and only on
the values wide enough to lose digits. An optional price spells "no value" two
ways - the field absent, or present and `null` - and both decode as absent.

The line is money, not "every decimal". String-only, everywhere it decodes:
the order, execution, account and market-data frames (submits, amends, order
updates and fills, `AccountState` with its balances, positions and margins,
order-status snapshots, trades and quotes); the `risk` block `GET /account`
publishes; and the opening balances in a `POST /accounts` body.

Still taking bare numbers, deliberately: `POST /control/divergence`, the
account policy inside a `POST /accounts` body, and the TOML run config. Those
are operator-supplied fractions and thresholds - a havoc probability, a
drawdown limit, an instrument multiplier - most of them spelled in TOML as
well as JSON, and none of them a quantity the venue books.

The order types the venue serves: Market, Limit, StopMarket, StopLimit,
TrailingStopMarket, TrailingStopLimit, MarketIfTouched, LimitIfTouched and
MarketToLimit. That is every order type nautilus expresses; none is refused.

A `TrailingStopLimit` carries two offsets and no price. `trail_offset` is how
far its trigger sits from the extreme the tape has reached, as on a
`TrailingStopMarket`. `limit_offset` is how far its limit sits from that
trigger, on the side the order can fill from - a sell rests at
`trigger - limit_offset`, a buy at `trigger + limit_offset`. The venue derives
the limit price from those and re-derives it every time the trigger ratchets and
every time you amend the trigger, so never send a `price`: it is refused at
submit and on a modify alike, because the first ratchet would overwrite it.
Amend the trigger and the venue reports the rederived limit back on
`OrderUpdated`.

Use it over `TrailingStopMarket` when you want a floor on the exit. Normally the
two behave alike, since a print that reaches the trigger is usually through the
limit as well. The difference shows when the tape gaps past both: the trailing
stop market takes whatever the gap offers, while the trailing stop limit rests
and waits rather than trading through your limit.

`post_only` - reject rather than take liquidity - is legal on `Limit`,
`StopLimit`, `LimitIfTouched` and `TrailingStopLimit`, and refused on every
other type. Those four are the ones whose purpose is to rest. It is refused on
`MarketToLimit` even though that type rests a remainder as a limit, because its
first act is to take what the touch offers, which is exactly what `post_only`
forbids. The refusal names the legal set rather than stating a rule, so a consumer
reading it does not have to infer which types it meant.

A `MarketToLimit` may carry any time in force, and the time in force
governs the remainder: `Fok` rejects the order before acceptance, `Ioc` cancels the
remainder, and `Gtc`, `Day` and `Gtd` keep it. The type's own doc argues it
exists because an IOC market cannot rest a remainder; that argues about why the
type exists, not about which time in force it may carry, so the combination is
admitted rather than refused.

A `MarketToLimit` takes the market and keeps the limit, which is both halves of
its name. Its first act is priced off the tape exactly as a market order's is -
the last print, slipped adversely by the fill band - except that its own stated
price bounds what it pays: a buy never fills above its limit, a sell never below
it. If the touch is short of the limit - a buy limited at 100 against a print of
101 - nothing is taken and the whole quantity rests, because the consumer asked not
to trade through that price. Marketability is judged against the band-drawn
trigger rather than the stated price, exactly as a `Limit`'s is, so a touch
inside the limit but outside that draw also rests; the two types answer the
question the same way on purpose, and neither promises the stated price alone
decides it. Whatever is not taken rests as an
ordinary limit at the stated price and is swept, filled and expired like one,
subject to the time in force above.

That is a change of behaviour as of 2026-08-19, and it is stated rather than
quietly corrected because a consumer testing against the old venue saw the
mismatch: the fill used to take the whole quantity at the order's own stated
limit with no reference to the tape - a buy limited at 200 against a last print
of 100 filled at 200 - so no remainder arose on the clean path at all, and where
an armed `PartialFillNext` manufactured one, the kept remainder rested inert,
offered to no sweep, unable to fill or expire.

One carve-out, and it is a real difference: a `MarketToLimit` submitted as an
order-list child never takes the market. A held child is released by its
parent's fill, and a release rests the child at its stated price - the same
thing it does for every non-conditional type, because the release happens inside
the linkage pass with no market reading of its own to price against. So a
market-to-limit exit leg is, in practice, a limit exit leg. That is stated here
rather than corrected because the alternative - handing the release a reading so
a released child can execute on arrival - is a change to what an order-list
child means, and `validate_order_link` refuses a `Market` child on exactly the
argument that a released child rests.

Order lists are served, so a genuine bracket needs no workaround. See
[Order lists](order-lists.md) for the rules and what each one does.

Time in force: Gtc, Ioc, Fok, Day and Gtd. A conditional may be Day or Gtd but
never Ioc or Fok - an order that must fill immediately cannot also wait for a
trigger. `Gtd` carries an `expire_time`; `Day` does not, because its expiry is
the instrument's own session close rather than anything a consumer states, and an
instrument with no calendar never expires one.

An expired order reports `OrderExpired`, not `OrderCanceled`, and its terminal
status on an order query is `Expired`. Match on it: an order you cancelled and
an order whose stated lifetime ran out are different outcomes, and a consumer that
folds both into "cancelled" cannot tell a venue action from its own time in
force. A nautilus host sees the distinction as `OrderStatus::Expired` and an
`OrderExpired` event.

A hedging reduce-only order must name the `position_id` it reduces. Without
one, "reduce whatever I have" is ambiguous when several independent or
opposing positions exist, so the venue rejects the submit instead of assigning
a fresh position id that cannot refer to an existing position.

Set it in your run config:

```toml
oms_type = "hedging"
```

`/health` reports the run's active `oms_type`, so a consumer connecting to a
venue it did not configure itself can confirm which mode it landed on before
trading.

## The venue does not gate on your consumer's configuration

On the nautilus side, `MogwaiExecClientConfig` carries its own `oms_type`
(matching your strategy's OMS style) and its own `account_type` (defaulting
to `Cash`). **mogwai never refuses a connection over either of these.** A
consumer configured for hedging can trade against a netting-mode run and vice
versa; a consumer configured with a cash account can trade a futures instrument
that posts margin. The venue is authoritative for its own book regardless of
what the connecting consumer declares about itself.

That permissiveness has one real consequence worth knowing about rather than
discovering: nautilus' `CashAccount` has no storage for margin balances, so a
consumer left on the default `account_type = "cash"` while trading a futures
instrument will see the venue's reported margin rows dropped on its own side.
The venue still posts and reports margin correctly - `/account`, the account
snapshot on the wire, and the adapter's forwarded `MarginBalance` rows are all
correct - the consumer simply has nowhere local to keep what it receives. If you
are trading futures instruments and want your own nautilus account object to
carry margin, configure `account_type = "margin"` on the exec client. mogwai
will not do this for you and will not refuse you if you don't.
