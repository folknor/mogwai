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

The clock keys are `sim_epoch_ns` (zero keeps the identity wall clock),
`wall_anchor_ns` (zero anchors at boot) and `speed`. `server_heartbeat_ms`
sets the server-originated liveness cadence; zero disables it.

The fill band is `fill_band_vol_mult` and `fill_band_max_ticks`. Every resting
limit draws a trigger price uniformly from `0 ..= band_ticks` ticks away from
its stated price, where `band_ticks` is `fill_band_vol_mult` times the tape's
trailing realized volatility scaled to a 60-second horizon, clamped to
`fill_band_max_ticks`. `fill_band_vol_mult = 0.0` degenerates to a strict
through-at-the-stated-price fill. The default `0.5` is the smallest multiplier
in the calibration sweep (`mogwai-server`'s `fills::vol_probe`) whose median
implied band lands in the usable 3-to-100-tick window on the default BTCUSDT
profile: a 9-tick median, 18 ticks at p90. `fill_band_max_ticks` defaults to
`200`, just above that 100-tick ceiling of usefulness. `fill_sweep_interval_ms`
is how often the run re-checks its resting limits against the tape; the sweep
is the only thing that ever fills a resting limit or delivers a market order's
slipped fill unsolicited, so boot refuses a zero interval.

One optional `[instrument]` table defines the run instrument. Omitting it uses
the built-in BTCUSDT profile. `[[instrument]]` is not accepted. `[regime]`
selects the single run-wide market regime. `[balances]` funds the one ledger.

The replay and admission settings remain run-wide: `gap_cap_ms`,
`fanout_depth`, `zero_speed_stall_ms`, `exec_held_budget_bytes`,
`admission_lane_frames`, `pending_command_acts`, and
`global_pending_command_acts`. There are no account, tape-cap, subscription,
or transport-profile configuration keys.
