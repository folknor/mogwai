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

Execution output that no command asked for reaches every open socket. The
venue serves four order types - `Market`, `Limit`, `StopMarket`, `StopLimit` -
and a resting order is one of three explicit states: a live limit, an
untriggered conditional, or an inert market remainder left by a partial fill
that is never scanned again and ends only on cancel. Every resting limit
carries a trigger price drawn once at submit from a seeded, volatility-scaled
band around its stated price (`fill_band_vol_mult = 0.0` degenerates to a
strict through-at-the-stated-price fill), and it fills only when the run's
fill sweep walks a print STRICTLY THROUGH that trigger; the fill is delivered
to each connection's lanes from the run, not from a command response. A
market order slips the same way, adverse to its side, off the same seeded
band. A conditional (`StopMarket`/`StopLimit`) rests untriggered until the
same sweep walks a print that TOUCHES its stop price - the mirror-image
predicate, since a stop holds no queue position and every real venue fires one
on touch rather than through. On trigger the venue emits `OrderTriggered` and,
in the same batch, either fills a stop-market at the triggering print slipped
by the same band, or promotes a stop-limit to a live limit judged against that
same print (filling at once if already marketable, resting with a fresh band
otherwise - it does not manufacture a fill through a gapped print). Reduce-only
and post-only are first-class wire flags: reduce-only clamps every fill to the
position it would close and cancels the order once that position is gone;
post-only rejects an order, at submit or at trigger, that would take liquidity
rather than filling it. Trailing stops and two-leg brackets are refused by
name; the venue models neither trailing state nor order linkage. No order book
exists: orders never interact, so self-trade within one account is impossible
rather than prevented, and every fill is judged only against the tape.

A run draws one 64-bit seed at launch, or takes it from config, and every
random stream in the run - the tape generator's and the fill band's - derives
from it by domain-separated derivation; nothing else in a run is random. The
tape's origin is the fixed constant `TAPE_ORIGIN_NS = 0`; warmup is generated
before readiness and the run proper begins at `run_start_ns = TAPE_ORIGIN_NS +
warmup_ns` on the same axis, so `data_origin_ns` is always `TAPE_ORIGIN_NS` and
history outside `[data_origin_ns, sim_now]` is refused. A run is therefore a
pure function of `(seed, config)` for a given build and fingerprint - with the
limit that a new seed only draws a new path from the one fitted model behind
the fingerprint, so marginalizing over seeds reduces variance conditional on
that model rather than adding out-of-sample market evidence. A declared
duration starts at `run_start_ns`, not boot. At its deadline the server
announces `RunComplete`, closes WebSockets normally, drains, and exits zero.

The protocol crate owns every JSON type shared by server and adapter. The
adapter uses WebSocket streaming only; `/trades` remains a request endpoint,
which is how history and warmup are fetched.
