# mogwai run configuration

Pass a TOML file explicitly with `mogwai serve --config PATH`; omission uses
built-in defaults. Unknown keys and malformed values fail startup.

The lifecycle keys are `run_duration_ns` (zero means no declared completion)
and `warmup_ns` (the uniform servable span before the run starts). `warmup_ns`
was formerly `backfill_horizon_ns`: an operator carrying an old file renames the
key and keeps the value, since the span it names is the same one. The boot
river is materialized before readiness and every other river on first read.
`/clock` names the resulting `data_origin_ns` and `warmup_ns`;
`/trades` and `/quotes` refuse a start below the floor or beyond current
simulated time, and clamp an end past current simulated time to it. They also
refuse with `400` a symbol not served by this run and name the served symbol,
so an impossible request stays distinguishable from a quiet market. History
symbols match the served instrument exactly, case included, even though config
resolves preset names case-insensitively. The served spelling is the one the
config wrote; clients should read it from `/instruments` rather than type it.
The readiness record names no symbol. This refusal concerns only this run's
symbol, never whether a preset exists for the requested string. `/quotes`
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
wall-clock input to a run's identity left: the only clock key is `speed`,
which paces delivery against wall time but never decides which tick is
served. `speed = 0.0` is
unpaced delivery, not a stopped clock - the underlying sim time still advances
at wall rate. `server_heartbeat_ms` sets the server-originated liveness
cadence; zero disables it.

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
`fills::vol_probe`: it samples 128 readings at a 10-minute stride, requires no
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

Instrument resolution has three layers: a preset bundle, default knobs from
`[instrument]`, then knobs from the matching `[symbols.<SYM>]` table. The
top-level `symbol` selects the river receiving the live paced tape; if absent, the default
bundle's BTCUSDT symbol stands. An explicit per-symbol `preset` beats a default
`[instrument]` preset, which beats a preset matching the symbol, which beats the
BTCUSDT default. Symbol-table lookup is ASCII case-insensitive, and boot refuses
two table keys that differ only in case. `[instrument].symbol` is refused:
overlays carry knobs, while the top-level key carries the boot symbol.

Boot resolves and validates every shape the config can reach, funding
currencies included. Every configured shape is reported by `/instruments` and
is servable through history; only the boot shape has a live paced tape. The
first history request for another cold river synchronously materializes its
checkpoint chain through the requested instant, so it can be slow and allocate
up to that river's checkpoint ceiling. A malformed
or unfunded table refuses startup even when the run would never serve it, so a
typo cannot wait to surface as a runtime rejection.

`[regime]` selects the single run-wide market regime. `[balances]` funds the one ledger.
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

The top-level `symbol` may be the only instrument-facing key. It selects a
matching shipped preset or the BTCUSDT default, and the derived definition
supplies the class, precision and increments. Overlay keys are logged explicit choices:
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

The symbol selects a matching committed preset, or BTCUSDT when unmatched.
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
speed uses the configured default. Speed is finite and non-negative and is
quantized to micro-multiples, so `100` and `100.0000001` share. One river can
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
