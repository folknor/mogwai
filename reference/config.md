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

The fill gate is `penetration_ticks` (zero, the default, fills a limit on
submit at its own price) and `fill_sweep_interval_ms`, which is how often the
run re-checks its resting limits against the tape. With the gate on, that sweep
is the only thing that ever fills a resting limit, so boot refuses a zero
interval.

One optional `[instrument]` table defines the run instrument. Omitting it uses
the built-in BTCUSDT profile. `[[instrument]]` is not accepted. `[regime]`
selects the single run-wide market regime. `[balances]` funds the one ledger.

The replay and admission settings remain run-wide: `gap_cap_ms`,
`fanout_depth`, `zero_speed_stall_ms`, `exec_held_budget_bytes`,
`admission_lane_frames`, `pending_command_acts`, and
`global_pending_command_acts`. There are no account, tape-cap, subscription,
or transport-profile configuration keys.
