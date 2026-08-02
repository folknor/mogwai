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
band. The trailing-volatility reading is cached once per symbol and fill-sweep
interval, using the interval's simulated-time floor, so a command burst shares
one coherent coarse band instead of repeating the full 300-second synthesis.
A conditional (`StopMarket`/`StopLimit`) rests untriggered until the
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

The generated tape publishes raw fills, not aggregated trades. One parent
match event updates the latent market once and emits a same-side sweep of one
or more children, one microsecond apart, walking monotonically in the take
direction. Consumers that count ticks therefore count raw fills. At the new
cadence a 300-second fill-band window carries about 15,700 returns; the L2
probe observed zero cold or budget refusals in 128 readings spanning ~21
simulated hours, closing the former cold-reading defect without changing the
estimator.

The same probe reports the other side of that shift, and the fill band was
re-calibrated against it. The estimator's 300-second window now carries ~15,700
returns where it carried ~32, so the horizon return it reads rose by two orders
of magnitude: at the former `fill_band_vol_mult = 0.5` the implied band came out
a median 439 ticks with a p90 of 703, above the 200-tick `fill_band_max_ticks`
clamp at nearly every instant. A clamp-saturated band is drawn uniformly across
the whole clamp range no matter what the tape is doing, which is the mirror
image of the inert `0.0` band - in neither case does the tape decide the fill.
The shipped default is now `0.005`, the smallest multiplier whose median implied
band falls inside the probe's usable 3-to-100-tick window (median 4 ticks, p90
7). The probe is the provenance: re-run it if the fingerprint or the cadence
moves again, and re-bless the fill golden, whose banded scenario reads the
default rather than restating it.

Reading the market at a submit is correspondingly more expensive: the walk costs
about 12.6 ms, so acceptance-time readings are memoized per symbol per
fill-sweep interval and a submit sees a reading that may be up to one interval
stale. A market order therefore fills at or beyond the market as of that
reading, not as of the fill instant.
