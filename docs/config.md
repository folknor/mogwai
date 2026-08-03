# mogwai run configuration

Pass a TOML file explicitly with `mogwai serve --config PATH`; omission uses
built-in defaults. Unknown keys and malformed values fail startup.

The lifecycle keys are `run_duration_ns` (zero means no declared completion)
and `warmup_ns` (simulated history generated before readiness). `warmup_ns` was
formerly `backfill_horizon_ns`: an operator carrying an old file renames the key
and keeps the value, since the span it names is the same one - what changed is
that the venue now MATERIALIZES it at boot instead of merely permitting requests
into it. `/clock` names the resulting `data_origin_ns` and `warmup_ns`;
`/trades` refuses a start below the floor or beyond current simulated time, and
clamps an end past current simulated time to it.

`seed` (absent means a fresh `u64` is drawn at launch, capped at `i64::MAX` so
it round-trips through TOML) is the run's single source of randomness; the
tape generator's stream and the fill band's stream both derive from it by
domain-separated derivation, and nothing else in a run is random. The tape's
origin is the fixed constant `TAPE_ORIGIN_NS = 0`; the run proper begins one
`warmup_ns` later on the same axis, so a run is a pure function of `(seed,
config)` for a given build and fingerprint. There is no wall-clock input to a
run's identity left: the only clock key is `speed`, which paces delivery
against wall time but never decides which tick is served. `speed = 0.0` is
unpaced delivery, not a stopped clock - the underlying sim time still advances
at wall rate. `server_heartbeat_ms` sets the server-originated liveness
cadence; zero disables it.

The fill band is `fill_band_vol_mult` and `fill_band_max_ticks`. Every resting
limit draws a trigger price uniformly from `0 ..= band_ticks` ticks away from
its stated price, where `band_ticks` is `fill_band_vol_mult` times the tape's
trailing realized volatility scaled to a 60-second horizon, clamped to
`fill_band_max_ticks`. `fill_band_vol_mult = 0.0` degenerates to a strict
through-at-the-stated-price fill. The default is `0.005`, selected by
`fills::vol_probe`: it samples 128 readings at a 10-minute stride, requires no
more than one percent cold-window refusals (currently zero), and picks the
smallest multiplier whose median implied band lands in the usable 3-to-100-tick
window. On the committed BTCUSDT fingerprint `0.005` reads a median band of 4
ticks and a p90 of 7. It replaced `0.5`, which was fitted to the print-layer
tape and implies a median 439 ticks at the raw-fill cadence - past the clamp, so
the band stopped tracking volatility at all. `fill_band_max_ticks` defaults to
`200`.
`fill_sweep_interval_ms`
is how often the run re-checks its resting limits against the tape; the sweep
is the only thing that ever fills a resting limit or delivers a market order's
slipped fill unsolicited, so boot refuses a zero interval.

One optional `[instrument]` table defines the run instrument. Omitting it uses
the built-in BTCUSDT profile. `[[instrument]]` is not accepted. `[regime]`
selects the single run-wide market regime. `[balances]` funds the one ledger.
`oms_type` is `netting` (the default) or `hedging`; the venue serves both and
refuses a client over neither, and `/health` reports the run's choice.

`account_id` names the account the run's single ledger is reported under,
defaulting to `MOGWAI-001`. It NAMES rather than selects: one venue is one run
is one ledger, so there is nothing to look up and no `?account=` to honour - a
query naming another account is ignored, not served. Set it because the
CONSUMER asserts it. A nautilus host holds an account of its own naming and
compares it against what the venue reports, so a venue insisting on its own
label is a venue that host cannot use.

The value must have the `ISSUER-NUMBER` shape, and boot is refused otherwise.
That is a nautilus rule rather than a wire rule - mogwai's own account type
accepts a bare word - but a nautilus `AccountId` cannot be constructed from one,
so a venue reporting `MOGWAI` boots cleanly, serves happily, and is rejected by
its consumer with an error naming neither this file nor this key. Refusing at
load costs a line.

## The instrument class

`[instrument]` carries five shape fields - `symbol`, `price_precision`,
`size_precision`, `price_increment`, `size_increment` - plus a REQUIRED
`[instrument.class]` sub-table naming the class. Top-level `base` and `quote`
were replaced by that table and now refuse boot with a message naming it.

```toml
[instrument]
symbol = "BTCUSDT"
price_precision = 2
size_precision = 8
price_increment = "0.01"
size_increment = "0.00000001"

[instrument.class]
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

## Margin, fees and the calendar

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

`preset = "MNQ"` inside `[instrument]` merges a committed, embedded preset -
`MNQ`, `MES`, `BTCUSDT`, `ETHUSDT`, `SOLUSDT`. Every other key must then be
stated under `[instrument.override]` as a dotted path
(`"class.multiplier" = "3"`); restating one at the top level refuses boot, and
so does overriding a path the preset does not set. Each override is logged at
boot with both values. Every preset carries a `[provenance]` map with one
entry per knob it sets - `fitted`, `derived` or `declared` with a rationale -
and boot refuses a preset that leaves any knob undeclared. `mogwai presets`
lists them; `mogwai presets MNQ` prints one with its provenance.

The replay and admission settings remain run-wide: `fanout_depth`,
`zero_speed_stall_ms`, `exec_held_budget_bytes`, `admission_lane_frames`,
`pending_command_acts`, and `global_pending_command_acts`. There are no
account, tape-cap, subscription, or transport-profile configuration keys.

The built-in generator profile expresses cadence with
`mean_event_duration_s`, `children_mean`, `children_single_frac`, and
`levels_mean`. Raw-fill size is expressed as `typical_notional`; its base-size
median is derived from the configured start price and the fitted lognormal
shape. The default `fanout_depth` is 65,536. A custom value should exceed one
wall second of projected frames:
`children_mean / mean_event_duration_s * speed`.
