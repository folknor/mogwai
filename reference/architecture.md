# mogwai architecture

Mogwai is a one-run fake venue. A direct launcher starts one foreground process
and receives a versioned readiness record through an inherited file descriptor.
The process binds one endpoint, owns one configured instrument, one generated
tape and one engine ledger.

The server exposes `/health`, `/account`, `/instruments`, `/clock`, `/trades`,
`/quotes`, `/control/divergence`, and `/ws`. Order entry is WebSocket-only: the
`POST /orders` carrier went with the HTTP transport profiles. Each socket feeds
one bounded sequential dispatcher, so admitted commands reach the market read
and engine in socket arrival order even when their modeled act latencies differ.
The queue and a process-wide permit bound parsed command work before it reaches
the blocking pool or engine mutex, and a full bound is a visible
`AdmissionRejected` the engine never sees. Inbound frames and reassembled
messages are capped at `MAX_CLIENT_MESSAGE_BYTES`, 64 KiB, so a dependency
default no longer sets the venue's memory bound; an oversized frame ends the
connection. A WebSocket is
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
Sweep marks and settlement prices are separate exact-instant last-print reads;
the coarse band cache never supplies unrealized P&L.
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
post-only rejects an order, at submit, at a price amend or at trigger, that
would take liquidity rather than filling it. Trailing stops and two-leg brackets are refused by
name; the venue models neither trailing state nor order linkage. No order book
exists: orders never interact, so self-trade within one account is impossible
rather than prevented, and every fill is judged only against the tape.

An instrument is a bundle of knobs, not one fixed shape. Two classes are
selectable: a spot currency pair, and a cash-settled continuous future with a
contract multiplier, whole-contract quantities on the order path AND on the
tape, and no expiry or roll. The generator's size grid is multiplier-aware -
notional per unit is `multiplier * price`, and a contract draw is rounded half
away from zero and floored at one contract, so no print becomes the zero
quantity nautilus drops. `latent_size_median` is stated directly in the
instrument's native size unit and names the continuous lognormal center before
that grid is applied. The floor truncates its lower tail, so it is deliberately
not called the observed size median. `TAPE_PROTOCOL_VERSION` is 16; version 5
removed the quote-notional proxy whose value was actually arithmetic mean
notional and made the latent size distribution explicit, and version 6 repaired
the GARCH recursion's second moment. Version 7 added the observable top of book,
version 8 added the instrument session profile, and version 9 split stochastic
parent advancement from wire-object materialization. Version 9 deliberately
preserves the version-8 tape byte for byte, but changes the generation path and
therefore advances the process version under the unconditional versioning rule.
Version 10 landed the July 2026 MNQ TBBO fit: floor-aware child-count
conditioning (below the point where the quiet-hour multiplier would push the
mean child count under one, the conditional parameters are re-solved so the
configured unconditional targets survive the one-child floor; above it the
legacy path is byte-identical), a per-instrument `size_log_sigma` whose default
reproduces the shared crypto shape byte for byte, and the fitted MNQ preset
values with MES inheriting them loudly.

Version 11 repaired a unit mismatch in the session calibration. `vol_hour` had
been fitted at protocol 8 as a PER-MINUTE quantity from NQ one-minute bars but
is applied PER PARENT EVENT, and minute-scale volatility carries the per-parent
scale times the square root of the arrivals in that minute - so the fitted 3.4x
hourly swing compounded with the 27.5x arrival swing and left the generated
Asia and London sessions roughly five times too quiet at bar scale. Both
session arrays were refitted from the July MNQ TBBO corpus as one atomic group:
`intensity_hour` from inferred-parent counts conditional on the frozen
`dow_weight`, landing 14.5x peak-to-trough against the volume proxy's 27.51x
upper bound, and `vol_hour` as a per-parent robust scale, which comes out
nearly flat and slightly INVERTED - overnight parents individually move a
little more than cash-session ones, and the old curve's swing was almost
entirely the arrival-density double count. `vol_scalar` was re-solved under the
corrected arrays and ships declared rather than fitted: its pooled scale gate
passes while the per-seed minute-range envelope still fails, which is the
standing tail-shape evidence.

Version 12 repaired the protocol-12b arrival-frame calibration: integrated
families take the bare mean. No shipped preset declares the arrival seam, but
it moves outputs for `(config, seed)` pairs already expressible under 11.
Version 13 normalizes the decimal price before it is hashed into the fill-band
draw key, because `rust_decimal`'s serialized form carries the scale and made
the band a function of how the client spelled its price - `100` and `100.00`
drew different triggers and different slippage for the same order. Every
seeded fill trigger and market-slippage offset therefore moves at 13. Version
14 makes a scheduled calendar jump part of the `ReopenGap` crossing frontier,
so an arm inside a closure cannot be skipped forever. The protocol-12b
MECHANISM landing, long pencilled in for 13, now takes 15.

Each generated parent event publishes one BBO before its first trade. The book
has an exact positive integer-tick width and is centered, with one rounding, on
the drifted latent mid. Every child in the parent sweep shares that book. Parent
trades are displaced from the published midpoint, so the default one-tick width
and half-tick displacement print at the touch. Width, top sizes, and trade
displacement are separate per-instrument calibration seams; a measured TBBO
corpus supplies them for MNQ (and, by stated inheritance, MES) as of protocol
10, while the BTCUSDT preset remains explicitly uncalibrated because no quote
evidence covers spot. Displacement is not
capped by width: the published BBO is only the top level, so an aggressive
parent may print beyond the touch without making the book malformed. A connecting WebSocket
receives the current BBO snapshot before later tape frames, and the adapter
retains that snapshot until its host activates quote delivery.

The volatility innovation is standardized to unit variance before it reaches
that recursion. The `a0` derivation has always assumed this, but the innovation
was a raw Student-t whose variance is `df / (df - 2)`, so the true condition was
`a1 * E[z^2] + b1 = 1.115` and the process had no finite stationary variance: it
stayed bounded only by its own rails, ran 8.17x hotter than `vol_scalar`
claimed, and sat pinned at the variance cap 12.96 percent of the time. `a1`,
`b1` and `vol_scalar` were re-solved against the corrected condition.

Three rails are named separately because they answer to different things. The
GARCH state cap and the feedback-return ceiling bound the base process and are
NEVER scaled by a regime, so an armed divergence cannot raise the process's own
ceiling and change what it does after the divergence ends. The realized-return
ceiling is absolute and applies after session and regime scaling, so a
divergence is an output envelope. That ceiling is a stated product policy sized
against a measured maximum-strength envelope, not a fitted market quantity: as a
log return it permits about +5.13 and -4.88 percent in a SINGLE event, and it
does not bound cumulative movement over many events.

Two structural fidelity limits of the generated futures tape are stated here
because no parameter can remove them. First, the calendar-driven baseline has
no automatic reopen jump: a real session reopen prints a discrete gap where
the closed-hours information arrives at once, while the generated mid resumes
its random walk from where it halted. An explicitly armed `ReopenGap`
divergence can inject such a jump on a subscription's view, but the clean
baseline tape never produces one, so on it overnight gaps are absent and any
large single-minute range the tape does produce is a volatility-cluster tail
inside a session, not a reopen - a different phenomenology occurring at a
different time of day. Second, the session profile modulates intensity and
volatility by HOURLY factors, so within-hour structure (the opening minutes'
concentration at the cash open, the settlement flurry) is smeared uniformly
across each hour; the profile reproduces hour-scale contour, not
minute-scale texture.

Fingerprint ranges are corpus-labelled observations. They select defaults and
produce operator diagnostics, but never admit or reject an instrument. A
shipped preset must either produce no diagnostic or accept its stable code on
the matching provenance entry; exact-set validation also rejects stale
acceptances. Hard
generator validation is mechanism-derived instead. In particular, the latent
size center must not sit two orders of magnitude below the minimum tradable
quantity, volatility must sit below the GARCH sigma cap so the process is not
born clipped, and the tick must be representable at the declared precision.
Rail HEADROOM is diagnosed rather than gated: a universal ratio of scale to rail
would repeat the same mistake in another dimension, denying a legitimately
higher-volatility instrument. Return ceilings remain shared module-level process
shape: a coarse truthful grid is allowed to produce a stickier latent mid rather
than receiving an uncalibrated stress-tail lift.
The dimensionless `tick_return / vol_scalar` and its squared random-walk crossing
estimate are exposed as diagnostics for deriving event-price repetition; they
do not pretend to model sweep stepping, bounce, recentering, or explicit repeat
draws by themselves.

A future's ledger is single-currency and collateralized. There is no base leg:
a fill moves the position and the VWAP, a quantity-reducing fill books
`(fill - avg) * closed * multiplier` of realized P&L straight into the
settlement balance, and margin - `maintenance_per_contract` per open contract
plus `initial_per_contract` per resting non-reduce-only contract - is what the
account locks, reported per symbol as posted margin the adapter forwards as
nautilus `MarginBalance` rows. Reduce-only orders reserve nothing, which is
what makes two bracket legs against one position exclusive rather than
additive. The sweep pass marks every open futures position to the tape, strikes
every settlement instant the calendar crossed AT ITS OWN INSTANT rather than at
the sweep boundary, and emits exactly one account snapshot per pass, after the
mark, so no consumer sees a stale `mark_px`. Settlement moves the accumulated
difference into actual cash and resets the VWAP to the settlement price, which
is why a losing futures position drains an account rather than merely carrying
a worse unrealized number. A breach is `total_balance + unrealized <
maintenance` - the TOTAL balance, because the locked amount already IS the
maintenance requirement and subtracting it twice liquidates solvent accounts.

A scheduled close is configuration, not havoc: `[instrument.calendar]` names
the open windows of the week in exchange-local time, the generator jumps a
closure whole rather than emitting inside it, and a consumer can know about it
in advance. `ReopenGap` remains havoc and remains unscheduled. Fees are a
per-instrument maker/taker schedule; liquidity side is decided where the fill
is produced, so a resting limit the sweep fills is a maker fill even though the
same order would have been a taker fill had it been marketable on arrival. A
funded account must cover the fee as well as the notional or the margin: the
submit check, the amend check and the fill check all add commission to the
requirement, the first two against the worse of the two rates since which side
the order provides is not known until it fills. A venue-originated liquidation
is charged the configured schedule but never a client-armed `FeeSurcharge`.

Margin and fees are treated here as instrument identity, which is not how
markets work: real schedules vary by account tier and real CME margin varies by
product, volatility, portfolio and time. A fixed per-contract margin and a
fixed schedule are declared simplifications of the venue's model, not
descriptions of the market's.

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

The generated tape publishes BBO updates and raw fills, not aggregated trades. One parent
match event updates the latent market once and emits a same-side sweep of one
or more children, one microsecond apart, walking monotonically in the take
direction. Its BBO is emitted first at the parent timestamp and remains the
governing book for every child. Consumers that count trade ticks therefore count raw fills. At the new
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
8). The probe is the provenance: re-run it if the fingerprint or the cadence
moves again, and re-bless the fill golden, whose banded scenario reads the
default rather than restating it.

Re-run against tape protocol 6, whose GARCH repair moved realized volatility by
roughly 1.3x in RMS, the selection is unchanged: `0.001` reads median 0 and p90
1, `0.002` median 1 and p90 3, `0.005` median 4 and p90 8, `0.010` median 9 and
p90 16. `0.005` is still the smallest multiplier satisfying the median rule, so
neither the band nor the fill golden's banded half needed to move.

Reading the market at a submit is correspondingly more expensive: the walk costs
about 9.8 ms as of 2026-08-14 (median over 100 distinct buckets, release, host
`bygg`, measured by the ignored `read_market_latency_stays_within_submit_budget`
instrument, versus the 12.6 ms recorded before the checkpoint stride was
repaired). The residual replay is now a small part of that: the 300 s
volatility window is the walk, which is why cutting checkpoint positioning by
53x moved this number by only a few milliseconds. So acceptance-time readings
are memoized per symbol per
fill-sweep interval and a submit sees a reading that may be up to one interval
stale. A market order therefore fills at or beyond the market as of that
reading, not as of the fill instant.

## The workspace and the offline evidence toolbox

Seven crates. `mogwai-protocol` owns the wire types and the shipped launcher and
imports nothing else in the workspace. `mogwai-engine` is the venue-agnostic
exchange core. `mogwai-data` owns `TickSource`, the k-way merge and the
`GeneratedSource` synthetic generator fitted to the committed fingerprint.
`mogwai-server` is a library - it owns the sockets, the clock and the replay
pacing, and ships no binary of its own. `mogwai-cli` is the `mogwai` BINARY: a
clap dispatcher over `serve` (which does no work itself, just forwards to
`mogwai_server::serve`) plus every offline subcommand - `gen`,
`tick-composition`, `presets`, `man`, `preflight`, `measure`, `fit`, `cache`,
`synth fingerprint`/`synth cadence` and `cadence-feasible`. `mogwai-adapter` is
the lone nautilus-dependent crate, unchanged by anything below.

`mogwai-lab` is the fifth non-adapter crate: the corpus-to-fingerprint method
library the 2026-08 Python-to-Rust rewrite absorbed from `analysis/` (the
rewrite program's phase records and per-script scope rulings are retired to
git history) - streaming TBBO/Binance-trades
parsing, the protocol-12a measurement engine, aggregation and bootstrap,
fingerprint and cadence synthesis, and the protocol-11 session-calibration
fit. Its dependency direction is one-way and asymmetric: `mogwai-lab` depends
on `mogwai-data`, `mogwai-protocol` AND `mogwai-server` (session-summary work
needs to resolve an `InstrumentProfile` through `Config::load` exactly as the
Python's `--config` scratch walks did), but `mogwai-server` depends on none of
it - there is no cycle, and `mogwai-lab` stays out of the tape-generation path
`TAPE_PROTOCOL_VERSION` scopes, the same reason `measure12a.rs` was
consumer-only inside `mogwai-server` before the rewrite moved it. `mogwai-cli`
depends on `mogwai-lab` for the pieces that need no server preset resolution
(preflight, cache, most of measure/fit/synth) and calls straight into
`mogwai-server` for the generated side of measurement.

THE INSTRUMENT SET IS OPEN, and that is why `mogwai-lab` is a library rather
than a folder of scripts. A symbol is a request string, never an admission
identity. `InstrumentDef` is derived through one path from the symbol and the
operator overlay: an explicit preset, a matching preset, or the BTCUSDT default
bundle. No second hardcoded default bundle exists, and no symbol is refused for
wanting a fit. The three shipped presets - MNQ, MES and BTCUSDT - are the
current state, not the end state. The intake sequence makes a tape better:

Config declares no instrument. It supplies a default knob overlay and optional
case-insensitive per-symbol overlays for total symbol resolution. The top-level
boot symbol is a slice-1 lifecycle artifact while one run still serves one
symbol; slice 2 moves that symbol to each request and retains the boot value as
the default for a request that carries none.

survey what cheap data exists, decide whether a paid corpus is worth buying
and which windows of it, buy, preflight, measure, characterize, fit, ship a
preset with its provenance. The offline toolbox is that sequence made
reusable, and the two consequences bind anything built on it. A component is
SPENT only when its QUESTION cannot recur, never merely because the MNQ pass
answered it - an archive inspector or a corpus driver is idle between
instruments, not dead. And per-instrument knowledge belongs in config or a
preset rather than a hardcoded list in the method: a preset tuple naming
today's three symbols is a defect the fourth exposes. The corollary for
evidence is that a finding measured on one instrument is one observation, not
a law, until a second instrument either reproduces it or does not - which is
why methods a preregistered test rejected are kept runnable rather than
deleted.

THE SECOND CONSEQUENCE IS THE DIRECTION OF TRAVEL, NOT A MET INVARIANT, and
stating it as met would be false. The offline toolbox still fixes
per-instrument choices in source, faithfully mirroring the Python it was
ported from rather than introducing the debt: `cadence.rs` fixes the pair set
and the archive month and takes BTCUSDT as anchor, `fingerprint.rs` takes
XBTUSD as anchor, and both `session_profile.rs` entry points resolve the MNQ
preset. None of these is reachable as an input. Retiring the Python removes no
parameterization that exists today - it was equally hardcoded - so closing
this is forward work rather than a porting debt, and it is what a second
instrument will force.

THE PARITY CONTRACT a port is held to, stated once because every case
otherwise gets argued from scratch: for every VALID input, the Rust must
either produce output equivalent to the implementation it replaces or embody
an explicitly approved semantic change. It MAY additionally reject inputs
outside the declared input contract. It may NOT silently accept malformed
input, and it may NOT silently change results for valid input.

The line that follows from it, and the reason it is worth writing down: a
gate passing on the committed fixtures is evidence about those fixtures, not
proof of equivalence over the contract. So a Rust refusal where the original
proceeded is a loud narrowing and needs only to be recorded; a Rust result
that differs on some valid input the fixtures happen not to contain is a
silent divergence and must be fixed or approved; and a Rust default where the
original raised is silent acceptance of malformed input, the worst of the
three, because it manufactures an answer. Fixing the third class by making
the committed artifact pass again is not a fix - the repair needs a fixture
chosen to DISTINGUISH the implementations, or the blind spot survives.

The rewrite's parity gates are the porting program's whole verification
story: every absorbed Python computation is checked against a committed JSON
artifact - `mnq-fit-preflight.json`, the observed and generated halves of
`mnq-measure-12a.json`, `cadence.json`, `fingerprint.json`, `mnq-fit.json` -
typed-canon-identically, with named, individually-verified exclusions for
genuinely live fields (wall-clock cost, the binding harness commit) rather
than a blanket tolerance. The gates live under
`crates/mogwai-lab/tests/parity3a*.rs`/`parity3b*.rs` and
`crates/mogwai-cli/tests/parity12a*.rs`/`parity3b.rs`, `#[ignore]`d because
they need local corpus or archive state on disk, and are excluded from
`brokkr.toml`'s complete profile by the shared `parity12a_`/`parity3a_`/
`parity3b_` naming prefix. The program-level review dossier - every gate,
every pinned cross-language convention (compensated float summation,
insertion-ordered accumulation, the ported CPython float repr and Mersenne
Twister, and the rest) and every owner decision the review adjudicated - is
retired to git history; the review signed and the program is complete.

The storage policy `mogwai_lab::storage` implements keeps three classes of
on-disk data apart, never mixed. ARTIFACTS (preflight, measurement and fit
outputs) are the user's files: written to `--out` or a subcommand's own
working-directory default, never cached, never auto-deleted. CACHE
(recomputable, keyed data - walk summaries, measure12a walk records) lives
under `$XDG_CACHE_HOME/mogwai/` (falling back to `~/.cache/mogwai/`),
overridable by `MOGWAI_CACHE_DIR` or `--cache-dir`, keyed by a
`ProvenanceToken` folding in the crate version, `TAPE_PROTOCOL_VERSION`, the
fingerprint hash, the full invoked command line, the measurement
sub-contract hash and (when built from a tree) the git sha; entries under a
stale token are unreachable by construction and pruned automatically on
write, with `mogwai cache stats`/`clean`/`clean --stale` covering the manual
case. SCRATCH (per-run temporaries) is a run-scoped directory under the
cache root, deleted on clean process exit and safe to leave behind on a
crash. Repo development pins `MOGWAI_CACHE_DIR` to the Python-era
`analysis/out` layout so the phase 1-3b parity gates read the caches those
scripts already produced; that pin is not the installed default.
