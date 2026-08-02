# mogwai clock

`SimClock` maps wall time to simulated time for one run. The run clock starts
immediately before readiness, after eager warmup; warmup generation does not
consume declared run duration.

`warmup_ns` is the materialized simulated interval before `run_start_ns`.
`data_origin_ns` is its earliest servable instant. `/clock` reports both the
clock and the current simulated time. A process is not restarted in place:
launchers create a new run and obtain a new readiness record.
