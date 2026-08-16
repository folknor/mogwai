# mogwai run configuration

Pass a TOML file explicitly with `mogwai serve --config PATH`; omission uses
built-in defaults. Unknown keys and malformed values fail startup.

The lifecycle keys are `run_duration_ns` (zero means no declared completion)
and `warmup_ns` (the uniform servable span before the run starts). `warmup_ns`
was formerly `backfill_horizon_ns`: an operator carrying an old file renames the
key and keeps the value, since the span it names is the same one. The boot
river is materialized before readiness and every other river on first read.
`/clock` names the resulting `data_origin_ns` and `warmup_ns`, and its optional
`?symbol=` answers on that river's boat clock rather than the venue clock.
`/trades` and `/quotes` refuse a start below the floor or beyond the NAMED
RIVER's now, and clamp an end past it. They do not refuse an unfamiliar symbol:
resolution is total, so any wire-legal label names a river this run will serve.
What they still refuse with `400` is a label that is not a legal symbol, a
shape this run's `[balances]` cannot fund, a shape the resolved configuration
makes invalid, and a run that has already materialized its river cap.

Symbols are LABELS and match case-exactly on the wire, even though
`[symbols.*]` keys and preset names resolve case-insensitively - `mnq` and
`MNQ` are two distinct rivers with two distinct tapes. The readiness record
names no symbol, so a client takes the labels it wants from its own
configuration; `/instruments` reports the configured shapes unioned with every
river materialized so far. `/quotes`
returns only BBO publications whose `ts_event` lies in the inclusive requested
window. It does not synthesize a leading governing quote when the window begins
inside a parent burst; callers needing that earlier book request an earlier
start. The WebSocket feed does not have this boundary issue because connection
setup sends the current BBO snapshot before later tape frames.

`seed` (absent means a fresh `u64` is drawn at launch, capped at `i64::MAX` so
it round-trips through TOML) is the run's single source of randomness. The fill
band has one run-level stream, while every requested symbol gets its own tape
path derived from the seed and label. Two symbols therefore have genuinely
different tapes even when they resolve to the same shape. Nothing else in a run
is random. The tape's origin is the fixed constant `TAPE_ORIGIN_NS = 0`; the
run proper begins one `warmup_ns` later on the same axis, so a run is a pure
function of `(seed, config)` for a given build and fingerprint, and one served
symbol's tape is a pure function of `(seed, config, label)`. There is no
wall-clock input to a run's identity left: every boat is placed at that same
fixed origin whenever it boards, so a river's path does not depend on when, or
whether, anyone connects to it. The only clock key is `speed`,
which paces delivery against wall time but never decides which tick is
served - a boat placed some way into a run is therefore permanently behind the
venue clock, which is what `/clock?symbol=` exists to report. `speed = 0.0` is
unpaced delivery, not a stopped clock - the underlying sim time still advances
at wall rate. `server_heartbeat_ms` sets the server-originated liveness
cadence; zero disables it.

## Accounts that outlive their connection

An account is the CLIENT'S and outlives the socket that named it, so a client
returning with the same id resumes its own ledger. Two keys govern what that
means, and both are reported on the readiness record so a launcher never has to
infer them.

`reset_account_on_reconnect` (default `false`) decides whether a returning
client gets its ledger back or a clean one. False, the default, is what makes a
reconnect a continuation - kill a worker holding a position, start it again,
find the book where it was left. Set it true when a batch reuses ids across
independent experiments.

`account_ttl_ms` (default `0`, meaning never) is how long an UNATTENDED account
survives before the venue collects it. While unattended an account is FROZEN: it
is not swept, its positions do not mark, its funding does not accrue and its
policy cannot liquidate it. That is deliberate - mogwai exists to exercise a
client's live path, not to run an account nobody is trading - and it means a run
spanning a disconnect has a gap in its risk history. Set the TTL longer than the
slowest restart any consumer performs; a collected account is gone, and the next
socket presenting that id opens a clean ledger.

The span is WALL time rather than simulated, because a frozen account has no
simulated clock: the boat carrying one wound down with the last socket.

Two things happen when a socket returns to a frozen account. Every surviving
order resumes scanning from the RETURNING boat's clock, since the departed one's
frontier sits in the new cursor's future. And whatever the account held off the
river the new socket bound - resting orders, positions - is retired, because the
new session can neither see nor close it.

While a client is ATTACHED, an order on a symbol no cursor is reading is
cancelled rather than left resting: nothing could ever fill or expire it, and
the client is there to be told.

History synthesis is admitted fail-fast at four concurrent `/trades` or
`/quotes` requests per run. A fifth request receives `503` with `history
request capacity exhausted`; it is not queued ahead of order-entry market
readings on the runtime's blocking pool. A slot is held until the response has
been written, not merely until synthesis finishes, so the ceiling bounds
resident response bytes as well as CPU - near 41 MB across four full pages.
A client that pages history concurrently should retry a `503` rather than
treat it as fatal.

The fill band is `fill_band_vol_mult` and `fill_band_max_ticks`. Every resting
limit draws a trigger price uniformly from `0 ..= band_ticks` ticks away from
its stated price, where `band_ticks` is `fill_band_vol_mult` times the tape's
trailing realized volatility scaled to a 60-second horizon, clamped to
`fill_band_max_ticks`. `fill_band_vol_mult = 0.0` degenerates to a strict
through-at-the-stated-price fill. The default is `0.005`, selected by
a volatility probe: it samples 128 readings at a 10-minute stride, requires no
more than one percent cold-window refusals (currently zero), and picks the
smallest multiplier whose median implied band lands in the usable 3-to-100-tick
window. On the committed BTCUSDT fingerprint `0.005` reads a median band of 4
ticks and a p90 of 8. It replaced `0.5`, which was fitted to the print-layer
tape and implies a median 439 ticks at the raw-fill cadence - past the clamp, so
the band stopped tracking volatility at all. `fill_band_max_ticks` defaults to
`200`.
`fill_sweep_interval_ms`
is how often the run re-checks its resting limits against the tape; the sweep
is the only thing that ever fills a resting limit or delivers a market order's
slipped fill unsolicited, so boot refuses a zero interval.

Config does NOT declare the run's instrument. A run serves whatever symbol a
client asks for; what config supplies is the SHAPE each requested label
resolves to, and one boot label so a run has a river under a boat before it
announces readiness.

Instrument resolution has three layers: a preset bundle, default knobs from
`[instrument]`, then knobs from the matching `[symbols.<SYM>]` table. This
resolution is TOTAL - a label with no `[symbols.*]` table and no matching preset
name resolves `[instrument]` over the operator's `preset` or over BTCUSDT.
The top-level `symbol` names only the boot river, the one that receives a boat
at readiness; if absent, the default
bundle's BTCUSDT symbol stands. An explicit per-symbol `preset` beats a default
`[instrument]` preset, which beats a preset matching the symbol, which beats the
BTCUSDT default. Symbol-table lookup is ASCII case-insensitive, and boot refuses
two table keys that differ only in case. `[instrument].symbol` is refused:
overlays carry knobs, while the top-level key carries the boot symbol.

Boot resolves and validates every shape the config NAMES - the boot symbol and
every `[symbols.*]` table - funding currencies included, and refuses startup
over any of them. It additionally resolves each shipped preset and the
unconfigured fallback, but only RECORDS an unfundable settlement currency there
rather than refusing: barring a BTCUSDT-only operator over an unfunded USD
would make the venue harder to launch than to use. A request landing on one of
those barred shapes is refused at bind or at the history poll instead, naming
the currency to add to `[balances]`.

Every configured shape is reported by `/instruments`, and every river a socket
bind or a history poll materializes joins the list. Any river can carry a boat:
the first socket on it places one, so a live paced tape is not the boot river's
privilege. A run retains at most 256 materialized rivers and never evicts them.
The first history request for a cold river synchronously materializes its
checkpoint chain through the requested instant, so it can be slow and allocate
up to that river's checkpoint ceiling. A malformed
or unfunded `[symbols.*]` table refuses startup even when nothing ever asks for
that label, so a typo cannot wait to surface as a runtime rejection.

A `[symbols.NAME.class]` table names one of five `kind` values, and the choice
decides how holding the instrument moves the ledger rather than merely how it is
labelled:

- `spot` with `base` and `quote`. Credits the base asset as a CURRENCY BALANCE,
  which is right for crypto spot where the base is money you can spend.
- `equity` with `currency` and an optional `multiplier` (one share per contract
  by default). Held as a POSITION, paid for in cash. Not a spot pair with the
  ticker as its base: that puts shares in the ledger as money.
- `future` with `underlying`, `settlement_currency`, `multiplier` and
  `asset_class`. Whole contracts only.
- `perpetual`, the same fields plus `funding_interval_ns` (eight hours by
  default) and `funding_rate`, the fraction of notional the LONG pays the SHORT
  each interval. Negative reverses the direction, which is what a market
  trading below spot produces. Sizing may be FRACTIONAL, unlike a listed
  future, because that is what crypto perpetuals actually do.
- `inverse`, coin-margined: `settlement_currency` is what moves and
  `quote_currency` is what the contract is priced in. The two must differ.

`[symbols.NAME.margin]` takes `initial_per_contract`, `maintenance_per_contract`,
`breach_action`, and a `basis` of `per_contract` (the default) or `notional`.
Per-contract is a fixed amount of settlement currency whatever the price, which
is how CME states a performance bond. Notional is a fraction of notional, so the
requirement moves with the price - ten-times leverage is `initial = 0.1`. That is
the leveraged account forex, crypto margin and Reg-T equity margin need.

`[regime]` selects the single run-wide market regime. `[balances]` is the
OPENING balance every account is funded with when its client names none - not
the balance of one shared ledger. A client that wants its own size opens an
account with `POST /accounts`, naming an id and its balances; a connection that
never does is served under the default account on these values.
`oms_type` is `netting` (the default) or `hedging`; the venue serves both and
refuses a client over neither, and `/health` reports the run's choice.

`account_id` names the DEFAULT account - the one a connection naming none is
served under - and defaults to `MOGWAI-001`. Every socket may name its own with
`/ws?account=`, and `GET /account?account=` reports whichever ledger it names,
so this selects a default rather than declaring the venue's one account. Set it
because the CONSUMER asserts it: a nautilus host holds an account of its own
naming and compares it against what the venue reports, so a venue insisting on
its own label is a venue that host cannot use.

The value must have the `ISSUER-NUMBER` shape, and boot is refused otherwise.
That is a nautilus rule rather than a wire rule - mogwai's own account type
accepts a bare word - but a nautilus `AccountId` cannot be constructed from one,
so a venue reporting `MOGWAI` boots cleanly, serves happily, and is rejected by
its consumer with an error naming neither this file nor this key. Refusing at
load costs a line.

## The instrument class

A config may carry no instrument-facing key at all, and many do: an unnamed
label selects a matching shipped preset or the BTCUSDT default, and the derived
definition supplies the class, precision and increments. Write a
`[symbols.<SYM>]` table only for a label whose shape you want to differ from
that. Overlay keys are logged explicit choices:
they replace a knob the bundle sets, or add an optional section - `fees`,
`margin`, `calendar` - it leaves out. Top-level `base` and `quote` remain
invalid.

```toml
symbol = "BTCUSDT"

[symbols.BTCUSDT]
price_precision = 2
size_precision = 8
price_increment = "0.01"
size_increment = "0.00000001"

[symbols.BTCUSDT.class]
kind = "spot"
base = "BTC"
quote = "USDT"
```

`kind = "future"` takes `underlying`, `settlement_currency`, `multiplier` and
`asset_class` (`fx`, `equity`, `commodity`, `index`, `cryptocurrency`) instead.
A future is cash-settled and continuous: it has no base leg, no expiry and no
roll. Boot refuses a future whose `size_increment` is not exactly `1` or whose
`size_precision` is not `0` - a fractional contract has no meaning, and
nautilus hardcodes both on a `FuturesContract`. Tick value is not configurable:
it is `price_increment * multiplier`, so a config cannot contradict itself.
`[balances]` must fund the class's settlement currency, which is the quote for
spot and `settlement_currency` for a future.
Configured shapes and the default fallback are checked at boot. Every embedded
preset is also resolved then; a reachable preset whose currency is absent is
recorded as funding-barred and refused when a socket bind or a history request
selects it. The refusal names the symbol and the currency to add to
`[balances]`, and it arrives before any trading, so a runtime funds rejection
still means depletion and only depletion.

### Equity, and the conventions that go with it

`kind = "equity"` takes `currency` and an optional `multiplier` (one share per
contract, on every venue that lists shares). A share is a POSITION and never a
currency balance: buying it debits the whole notional and credits shares, which
is what makes short sales, settlement periods and round lots expressible at all.

Three optional keys carry the conventions:

```toml
[symbols.AAPL.class]
kind = "equity"
currency = "USD"
lot_size = "100"          # orders must be a whole number of lots
borrowable = "50000"      # shares this account may be short; 0 is hard-to-borrow
settlement_ns = 172800000000000   # T+2, held unsettled for two days of sim time
```

`lot_size` (default `1`, meaning odd lots are accepted) governs what may be
SUBMITTED. It is deliberately not `size_increment`, which stays at one share:
a partial fill legitimately leaves an odd-lot remainder that the grid still has
to represent.

`borrowable` is the LOCATE. Absent means the venue models no borrow constraint;
`0` states a name nobody will lend, and any other value caps how short the
account may go. It is checked against the account's NET position in the symbol.

`settlement_ns` holds sale proceeds unspendable for that span. The money is
credited immediately - it is the account's - and appears as `locked` on the
balance row until it settles, which is what `T+N` means to a strategy. It is a
fixed sim span rather than N sessions, so a weekend does not stretch it; a
preset wanting the session-counted form owes a calendar-aware successor.

**Cash account or margin account.** An equity with NO `[symbols.X.margin]` table
is a CASH account: it pays the full notional on a buy and may not sell short at
any price, which is refused by name rather than as a funding shortfall. Give the
symbol a margin policy with `basis = "notional"` and it becomes a MARGIN
account - `initial = "0.5"` and `maintenance = "0.25"` is Reg-T. It then posts a
fraction of the notional and borrows the rest, so the settlement balance goes
negative by the loan while the shares sit on the other side of it, and the
maintenance requirement is measured against the position's value at the mark.

## Margin, fees and the calendar

Every nested overlay form below is also legal under a symbol table: for
example, `[symbols.MNQ.margin]`, `[symbols.MNQ.fees.maker]`,
`[symbols.MNQ.calendar]`, `[symbols.MNQ.session]`, and
`[symbols.MNQ.override]`. The symbol-specific form applies after the matching
`[instrument.*]` default form.

`[instrument.margin]` is mandatory on a future and refused on a spot pair. It
takes `initial_per_contract`, `maintenance_per_contract` (positive, and no
greater than the initial - the reverse opens every position already in breach)
and `breach_action`, either `refuse` (the default: no new risk while equity is
below the maintenance requirement) or `liquidate` (the venue closes the
position through its own fill band). A future posts collateral rather than
reserving notional at every funds site: submit, fill and amend alike.

`[instrument.fees.maker]` and `[instrument.fees.taker]` each take
`basis = "basis_points"` with a `rate` in `0 ..= 1000`, or
`basis = "per_contract"` with a non-negative `amount`. A negative rate refuses
boot; rebates are not modelled. Omitting the table is the fee-free venue.
Commission books in the settlement currency and reaches the consumer on the
fill.

`[instrument.calendar]` expresses genuine closure, which the hour and day
weights of `[instrument.session]` cannot: those shape intensity WITHIN an open
session and must stay strictly positive. It takes `utc_offset_minutes`
(`-720 ..= 840`, fixed - DST is unmodelled), `open_windows` as sorted
non-overlapping half-open intervals in minutes from local Sunday 00:00 with at
most one wrapping past it, and an optional `settlement_minute_of_day` naming
the local minute the daily settlement price is struck. That minute must fall
inside an open window; a settlement cannot be struck on a shut market. While a
calendar reports closed, market orders and marketable limits are rejected with
`market closed`, resting orders persist and simply do not fill, and the mark
freezes at the last print before the close. No calendar table means always
open, which is the crypto case and the default.

## Presets

A REQUESTED symbol selects a committed preset of the same name, or BTCUSDT when
unmatched - so `?symbol=MNQ` gets the index-future bundle from a config that
never mentions MNQ.
An explicit `preset = "MNQ"` in either overlay takes precedence and serves that
bundle under the requested symbol. Top-level overlay keys are legal replacements or additions;
`[instrument.override]` or `[symbols.<SYM>.override]` reaches dotted paths such as
`"class.multiplier" = "3"`. Overriding a dotted path the bundle does not set
refuses boot. Each override is logged with both values. Every preset carries a `[provenance]` map with one
entry per knob it sets - `fitted`, `derived` or `declared` with a rationale -
and boot refuses a preset that leaves any knob undeclared. `mogwai presets`
lists them; `mogwai presets MNQ` prints one with its provenance.

The replay and admission settings remain run-wide defaults. `fanout_depth` is
applied to each boat's own ring; the remaining settings are run-wide:
`zero_speed_stall_ms`, `exec_held_budget_bytes`, `admission_lane_frames`,
`pending_command_acts`, and `global_pending_command_acts`.
`pending_command_acts` bounds one socket's sequential command queue;
`global_pending_command_acts` bounds queued or executing
commands across the run. A full bound produces a visible `AdmissionRejected`
without letting the engine see the command. There are no
account, tape-cap, subscription, or transport-profile configuration keys.

The built-in generator profile expresses cadence with
`mean_event_duration_s`, `children_mean`, `children_single_frac`, and
`levels_mean`. Raw-fill size is expressed as `latent_size_median` in the
instrument's native size unit. It is the continuous lognormal median before
minimum-size flooring, grid quantization, or round-lot snapping, so it must not
be read as the observed post-grid median. Explicit `top_sizes` values are
honored even with `uncalibrated` provenance; that provenance describes evidence,
not whether configuration is active. `trade_displacement_ticks` is only required
to be finite and non-negative; it is intentionally not capped at half
`quoted_width`, because the displayed BBO is one level and an aggressive parent
may print beyond the touch. The two quantities remain independent calibration
seams. The default `fanout_depth` is 1,048,576. At
boot, a custom value should exceed one wall second of BASELINE projected frames:
`children_mean / mean_event_duration_s * speed`.
An armed flow surge can exceed that baseline. Under the measured maximum surge,
the default holds only 0.114 wall seconds - longer than the 0.030 the previous
262,144 default held, but still far short of a wall second, so a surge-exposed
run should size this deliberately rather than inherit it.
`reference/performance.md` records the measurement under its protocol 8 section.

The fingerprint retains `mean_trade_notional` under its honest name for corpus
comparison; it is derived from the latent median, reference price, contract
multiplier, and lognormal shape and never feeds the sampler.

Websocket requests accept `?symbol=`, `?speed=` and `?duration_ms=`. An absent
symbol binds the boot river, which is what a client written before symbols
moved to the request does. An absent
speed uses the configured default. Speed is finite and non-negative, capped at
1,000,000, and quantized to micro-multiples, so `100` and `100.0000001` share.
One river can
carry one boat: a different quantized speed receives a `400` naming the speed
already seated. Duration is simulated milliseconds from boarding and belongs
to the passenger, not the boat, so passengers with different durations share.

Generator admission is based on mechanism constraints: positive finite values,
grid representability, coherent sweep probabilities, size units compatible with
the minimum tradable quantity, and volatility headroom below the GARCH cap.
The fingerprint's corpus ranges are diagnostics, not admission gates. An
operator configuration outside a fitted range logs a warning naming the corpus;
the committed preset test requires every shipped preset warning to be accepted
explicitly on the matching provenance entry using the
`accepted_diagnostics = ["outside-empirical-corpus-range"]` list. The test
compares the two sets exactly, so an unaccepted warning fails and a stale
acceptance fails after the warning disappears.
Whole-number products therefore use `price_decimals = 0` normally,
and a tick larger than Kraken's largest observed tick is not rejected merely
for being outside a crypto corpus.
