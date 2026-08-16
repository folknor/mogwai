# Netting and hedging: the venue serves both

mogwai's own run config selects an order-management style with `oms_type`,
either `netting` (the default) or `hedging`. This is a **run-level** choice,
not an instrument one - it applies to the whole venue for the run, the same
way `seed` or `speed` does, and every account the venue opens inherits it.
One ACCOUNT is one ledger: a client trading several symbols books every fill on
all of them into its own account, and the style below decides only how positions
are keyed within it. Two accounts on one venue never share a position book.

- **Netting** collapses every fill on an instrument into one position per
  symbol: an opposing fill reduces or reverses that one position, and a
  client-supplied position id is echoed back on the wire but is not used to
  key anything.
- **Hedging** keeps opposing fills as separate positions, keyed by
  `position_id`. An order submitted with no id opens a fresh venue-assigned
  one; a fill reports the id the venue actually booked it against, which
  under hedging may not be the id the client sent.

ORDER TYPES the venue serves: Market, Limit, StopMarket, StopLimit,
TrailingStopMarket, MarketIfTouched, LimitIfTouched and MarketToLimit. The one
refused shape is an ORDER LIST (OCO, OTO): the venue models no linkage between
orders, so a bracket where one fill cancels its sibling has to be two
independent reduce-only legs the strategy reaps itself.

TIME-IN-FORCE: Gtc, Ioc, Fok, Day and Gtd. A conditional may be Day or Gtd but
never Ioc or Fok - an order that must fill immediately cannot also wait for a
trigger. `Gtd` carries an `expire_time`; `Day` does not, because its expiry is
the instrument's own session close rather than anything a client states, and an
instrument with no calendar never expires one.

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

## The venue does not gate on your client's configuration

On the nautilus side, `MogwaiExecClientConfig` carries its own `oms_type`
(matching your strategy's OMS style) and its own `account_type` (defaulting
to `Cash`). **mogwai never refuses a connection over either of these.** A
client configured for hedging can trade against a netting-mode run and vice
versa; a client configured with a cash account can trade a futures instrument
that posts margin. The venue is authoritative for its own book regardless of
what the connecting client declares about itself.

That permissiveness has one real consequence worth knowing about rather than
discovering: nautilus' `CashAccount` has no storage for margin balances, so a
client left on the default `account_type = "cash"` while trading a futures
instrument will see the venue's reported margin rows dropped on its own side.
The venue still posts and reports margin correctly - `/account`, the account
snapshot on the wire, and the adapter's forwarded `MarginBalance` rows are all
correct - the client simply has nowhere local to keep what it receives. If you
are trading futures instruments and want your own nautilus account object to
carry margin, configure `account_type = "margin"` on the exec client. mogwai
will not do this for you and will not refuse you if you don't.
