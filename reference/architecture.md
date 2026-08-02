# mogwai architecture

Mogwai is a one-run fake venue. A direct launcher starts one foreground process
and receives a versioned readiness record through an inherited file descriptor.
The process binds one endpoint, owns one configured instrument, one generated
tape and one engine ledger.

The server exposes `/health`, `/account`, `/instruments`, `/clock`, `/trades`,
`/quotes`, `/control/divergence`, and `/ws`. Order entry is WebSocket-only: the
`POST /orders` carrier went with the HTTP transport profiles, leaving one
carrier and therefore no dispatch ordering to reason about. A WebSocket is
attached to the run tape on upgrade: clients do not subscribe or supply an
account identity. The bounded fanout ring remains; a lagging client receives
`FeedLagged` on the priority lane and is closed with WS 1011.

Execution output that no command asked for reaches every open socket. Every
resting limit carries a trigger price drawn once at submit from a seeded,
volatility-scaled band around its stated price (`fill_band_vol_mult = 0.0`
degenerates to a strict through-at-the-stated-price fill), and it fills only
when the run's fill sweep walks a print strictly through that trigger; the
fill is delivered to each connection's lanes from the run, not from a command
response. A market order slips the same way, adverse to its side, off the same
seeded band. No order book exists: orders never interact, so self-trade within
one account is impossible rather than prevented, and every fill is judged only
against the tape.

Warmup is generated before readiness. `data_origin_ns = run_start_ns -
warmup_ns`, and history outside `[data_origin_ns, sim_now]` is refused. A
declared duration starts at `run_start_ns`, not boot. At its deadline the server
announces `RunComplete`, closes WebSockets normally, drains, and exits zero.

The protocol crate owns every JSON type shared by server and adapter. The
adapter uses WebSocket streaming only; `/trades` remains a request endpoint,
which is how history and warmup are fetched.
