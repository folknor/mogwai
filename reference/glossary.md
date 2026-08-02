# mogwai glossary

- **Run**: one foreground mogwai process, one instrument, one tape and one
  ledger. A run may declare a simulated duration.
- **Tape**: the run-owned generated market-data sequence. It is warmed before
  readiness and broadcast to every connected WebSocket.
- **Ledger**: the single `mogwai-engine` instance owned by the run. HTTP and
  WebSocket order entry act on the same ledger.
- **Warmup**: the materialized simulated history from `data_origin_ns` through
  `run_start_ns`. `warmup_ns` is their distance.
- **ReadyRecord**: one versioned JSON line sent over the inherited ready fd.
- **RunComplete**: the terminal WebSocket announcement for a planned duration
  completion, followed by a normal close.
