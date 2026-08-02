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
