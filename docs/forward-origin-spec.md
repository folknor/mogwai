# Coherent forward-test data origin (spec)

Written against `reference/technical-implementation-spec.md`. Source item:
`../freedom/FINDINGS.md` #13 ("Forward warmup historical bars never arrive ->
fatal warmup timeout (identity clock)"), recorded here as `docs/todo.md` Next
item 3. Sibling spec `docs/coherent-clock-spec.md` (the accelerated-clock work)
is the upstream of the `SimClock` this spec builds on; the two are
reconciled in the Survey below.

## The defect in one sentence

The synthetic tape is anchored at a frozen `ORIGIN_TS` (2023-11-20) while the
identity clock advertises wall-now (2026), so a forward warmup request for
`[now-warmup, now]` lands ~1.6 years past the tape's start, blows the 50k
seek cap, and returns an empty page that broadarrow's warmup never completes.
The clock and the data are two timelines that disagree, and both are mogwai's.

## Survey of the ground

The two anchors and why they diverge:

- `crates/mogwai-server/src/source.rs:14` - `const ORIGIN_TS: u64 =
  1_700_438_400_000_000_000` (2023-11-20 00:00 UTC), the frozen tape start.
- `crates/mogwai-protocol/src/lib.rs:69` - `SimClock::sim_ns(wall) =
  sim_epoch + (wall - wall_anchor) * speed`. Identity (`sim_epoch=0`,
  `wall_anchor=0`, `speed=1`) returns wall unchanged. So in identity mode
  `/clock` advertises 2026 while the tape serves 2023.

The generator is the constraint that shapes every option:

- `crates/mogwai-data/src/generated.rs:366-428,531-556` - `GeneratedSource` is
  a forward-only path-dependent stream. `clock_ns` starts at `start_ts` and
  accumulates sampled ACD/Weibull durations; price is a GARCH walk with bounce
  state; the RNG is `StdRng::seed_from_u64(seed)` where `seed = seed_for(symbol)`
  alone (`source.rs:22-29`). The anchor (`start_ts`) is a pure additive offset on
  `clock_ns`; the draw SEQUENCE is a function of `(seed, step-index N)`, not of
  absolute time. There is no closed-form `time -> step` inverse: reaching an
  instant `T` from the anchor costs O(N) ticks. This single fact is what the
  seek cap, the horizon, and the uptime ceiling all are - the same O(span) walk
  wearing three hats.
- Committed cadence (`analysis/fingerprint.json:221-225`): `mean_duration_s`
  median 7.19s, range 3.75-12.56s. Scalars are per-symbol
  (`ConfiguredInstrument`, `main.rs:104-110`), so cadence - and therefore
  ticks-per-unit-time - varies by instrument. The fastest subscribed symbol
  binds every tick-budget conversion.

The two source builders, and the asymmetry between them:

- `source.rs:134-155` `build_history_source` - anchors `GeneratedSource` at
  `ORIGIN_TS`, wraps it in `BoundedSeek { cap: MAX_HISTORY_SEEK_TICKS }`
  (`source.rs:15` = 50_000), and `MergeSource::starting_at(.., start)`
  (`mogwai-data/src/lib.rs:266`) SEEKS to the requested `start`. A seek that
  exhausts the cap before reaching `start` returns `None` (`source.rs:167-184`)
  -> empty page.
- `source.rs:98-132` `build_live_source` - anchors `GeneratedSource` at
  `start_ts.unwrap_or(ORIGIN_TS)` and uses `MergeSource::new` (no seek). It
  RE-ANCHORS a fresh seed-head draw at the anchor rather than seeking into the
  shared tape.

Three consequences of that asymmetry, all confirmed in-tree:

1. **#13 itself.** History seek from `ORIGIN_TS` (2023) to a 2026 window drains
   the 50k cap (~2.2-7.3 days of tape) without catching up -> `None` -> empty
   `BarsResponse` (the response IS sent, even empty: `client.rs:614-625`).
2. **Warmup/live discontinuity.** Even with delivery fixed, warmup (seek into
   the `ORIGIN_TS` tape) and live (fresh re-anchor) are different realizations
   of the same seed, so the price level jumps at the splice.
3. **Reconnect price reset (same root, second site).** On WS reconnect the
   adapter re-subscribes with the advanced resume cursor (`advance_sub_start_ts`,
   `source.rs:1150-1161`); `build_live_source` re-anchors a fresh generator at
   that cursor with the symbol-seeded RNG, so the resumed stream's first tick is
   ~`start_price` again - the price walk restarts at 60000 on every reconnect.

The live pacing path makes the live-seek fix non-optional, not just nicer:

- `main.rs:864-900` `spawn_replay`. Accelerated (`!sim.is_identity()`) paces to
  an absolute `sim.wall_ns(tick.ts_event())` deadline - a tick whose deadline is
  already past emits with no sleep, so a generator anchored behind now FIREHOSES
  the backfill (the NOTED "catch-up dump", `FINDINGS.md`). Identity paces by
  relative inter-tick gaps - a generator anchored 24h behind now would replay
  24h of backfill at REAL-TIME gaps before emitting anything current, i.e. the
  live stream would be 24h stale for a wall day. So once the origin moves behind
  now, live MUST seek to sim-now in both modes.

The consumer and wire surfaces touched:

- `/clock` (`main.rs:459-461`) serializes `SimClock` directly; the adapter
  deserializes it in `fetch_clock` (`mogwai-adapter/src/clock.rs:354-371`),
  used by `mogwai_clock_factory` (`clock.rs:385`). Nothing today advertises the
  data origin, so a client cannot tell where the tape begins.
- broadarrow's warmup completion swallows an empty/short page:
  `research/broadarrow/crates/bridge/src/strategy/actor.rs:273`
  `let Some(first) = bars.first() else { return Ok(()) }`, and the fatal message
  lives in `.../strategy/feed.rs`. These are in the sibling `../broadarrow`
  repo and are named-and-excluded below.

## The decision

The fix is the coherence fix, taken on the data axis: **delete the frozen
`ORIGIN_TS` and derive the tape origin from the advertised clock at boot**, so
the venue's data timeline and its `/clock` cannot diverge. Concretely:

```
data_origin_ns = sim_now_at_boot.saturating_sub(backfill_horizon_ns)
```

shared by both source builders. The horizon is a single global config quantity
in time (default 24h); a wrong horizon is made LOUD (detect-and-refuse, below),
never a silent under-warm, which is why the default's exactness is low-stakes.

Forks (from the problem statement), resolved:

- **Fixed boot-derived origin: YES** (fork 1, but unfrozen). One `data_origin_ns`
  per boot, shared by every symbol's generator.
- **Lazy per-request origin: KILLED** (fork 2). It reintroduces origin drift -
  overlapping windows would be different realizations - destroying the single
  coherent tape this fix exists to protect. A per-symbol origin is fork 2 in
  spatial form and is killed with it; cadence differences affect only the work
  budget, never the origin.
- **The 50k cap: kept as a runaway backstop, not a design constraint.** With the
  origin tracking the clock, every legitimate request lives in
  `[data_origin, now]` by construction; the binding check becomes the analytic
  `start >= data_origin`, not "drain 50k and give up".
- **Origin-linear seek vs checkpointed seek: decided by measurement** (the one
  point the three reviews split on). Per `technical-implementation-spec.md` pin
  5, a throughput-justified choice leads with the instrument that prices it.
  Landing 1 measures; Landing 4 (checkpointed seek) proceeds or closes against a
  stated threshold.
- **O(1) block-addressable generator: DEFERRED** (named, out of scope). It is
  the only thing that dissolves the uptime ceiling AND an unbounded past, but it
  re-architects the generative model and forces a fingerprint golden re-baseline
  that #13 does not require. Checkpointing (Landing 4) lifts the ceiling while
  preserving the exact realization, so the block model is a separate future item.

## Target artifacts

Wire (`mogwai-protocol`): a new richer clock payload, leaving `SimClock` the
pure affine map it is.

```rust
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServerClock {
    pub sim: SimClock,
    pub server_now_ns: u64,      // sim.sim_ns(wall) at the moment of the request
    pub data_origin_ns: u64,     // earliest ts the tape can serve
    pub backfill_horizon_ns: u64,
}
```

Server (`mogwai-server`):

- `Config` gains `backfill_horizon_ns: u64` (serde `default`, value
  `86_400_000_000_000` = 24h), documented next to `sim_epoch_ns`.
- `AppState` gains `data_origin_ns: u64`, computed once in `main` as
  `sim.sim_ns(now_ns()).saturating_sub(cfg.backfill_horizon_ns)`.
- `build_history_source` and `build_live_source` take `data_origin: u64` and
  anchor the generator there; BOTH wrap in `BoundedSeek` and use
  `MergeSource::starting_at(seek_target)`. History `seek_target = request.start`;
  live `seek_target = start_ts.unwrap_or(sim_now_at_request)`.
- `trades` handler: when `start` is `Some(s)` and `s < data_origin_ns`, return
  `422` with a body naming `data_origin_ns` (analytic refuse, no drain). `None`
  start means "from origin" and is served. Degenerate-window behavior
  (`start>end`, `limit=0`, etc., the VERIFIED-OK cases) is unchanged.
- `clock` handler returns `Json<ServerClock>`.

Adapter (`mogwai-adapter`):

- `fetch_clock` returns `ServerClock`; `mogwai_clock_factory` uses `.sim`; the
  data client stores `data_origin_ns`.
- `request_bars` / `request_trades`: pre-check the requested window start against
  the stored `data_origin_ns` and fail LOUDLY (a logged, surfaced error naming
  both the requested start and the floor) instead of issuing a doomed fetch; and
  surface a server `422` as a clear error rather than the `if let Ok(..)` drop
  (`client.rs:598-626`).
- `subscribe_symbol` (`client.rs:130-131`): send `start_ts = None` and let the
  server default live to sim-now, removing the `sim_epoch_ns` anchor that drives
  the accelerated catch-up dump.

## Landings (ordered; suite green at every boundary)

### Landing 1 - Instrument: price the seek (measurement first)

The cap value and the checkpoint-vs-raise-cap decision are throughput-justified,
so the measurement is the first landing. Extend the existing perf test
(`main.rs:1153-1167`, `generated_history_default_limit_is_bounded_and_fast`)
into a reported measurement that prints, for the default profile AND a
fast-cadence (3.75s) profile:

- per-tick synthesis throughput (ticks/sec), and
- wall cost of a from-origin `BoundedSeek` of N ticks for
  N in {24h-of-tape, 52h-of-tape, candidate-cap}.

Thresholds the reading is judged against:

- Set `MAX_HISTORY_SEEK_TICKS` (backstop `C`) so the worst LEGITIMATE on-tape
  from-origin seek - a 24h warmup at the fastest cadence (~23k ticks) - stays
  under request-path budget `B = 100 ms`.
- Compute the accelerated uptime ceiling under `C` at `speed = 120`, fastest
  cadence (fresh-subscribe seek span = `horizon + wall_uptime * speed`, in
  ticks). Venue-contract floor `F = 2 h` wall uptime for fresh subscribes.
  - If a single `C` satisfies both `B` and `F` -> raised cap alone is the final
    design; **Landing 4 CLOSES as mispriced.**
  - If `B` and `F` are mutually unsatisfiable by any single `C` (the expected
    outcome: decoupling per-request cost from session length is exactly what a
    from-origin seek cannot do) -> **Landing 4 PROCEEDS.**

Gate command: `brokkr test -p mogwai-server --nocapture <measurement-name>`
(the runner already forces `--nocapture`; read the printed numbers). Keep/revert:
the measurement lands as a committed test regardless; its READING selects the
Landing 4 path.

**Reading (landed as `seek_throughput_measurement` in `source.rs`).** Synthesis
throughput is ~1.9M ticks/sec and cadence-independent (each tick is the same
GARCH+Weibull work). Realized cadences: default ~7.8s/tick, fast ~3.9s/tick.

- Worst legitimate on-tape warmup - 24h at fast cadence - is ~22k ticks (~11.7
  ms), comfortably under `B = 100 ms`. Sizing the cap to `B` gives
  `C_B = 0.1 s * 1.9M = ~190k` ticks.
- Honoring `F = 2h` fresh-subscribe uptime at `speed = 120`, fast cadence, demands
  `C_F = (horizon + F*speed)/cadence = ~244k` ticks (~129 ms at a C_F-deep seek).
- `C_F (244k) > C_B (190k)`: no single `C` satisfies both `B` and `F`. Under a
  `B`-sized cap the accelerated uptime ceiling is ~90 min, short of the 2h floor.

**Verdict: Landing 4 PROCEEDS.** This is the structural outcome the spec
predicted - a from-origin seek cannot decouple per-request cost from session
length. The numeric margin is throughput-sensitive (a much faster host could
raise `C_B` past `C_F` and flip the arithmetic), but the decoupling argument
holds regardless, so checkpointing is the durable answer.

### Landing 2 - Boot-derived origin + unified seek + refuse-straddle (server)

The core of #13. Delete `ORIGIN_TS`; add `backfill_horizon_ns` to `Config` and
`data_origin_ns` to `AppState`; thread `data_origin` into both builders; make
`build_live_source` wrap `BoundedSeek` + `starting_at(start_ts.unwrap_or(sim_now))`;
add the `422` straddle refusal to `trades`. Raise `MAX_HISTORY_SEEK_TICKS` to the
Landing-1 value as the backstop. After this landing identity mode is coherent and
#13 is functionally closed for any on-tape warmup.

Gates (EXACT commands):

- `brokkr check` - gremlins + clippy + all tests, changed-files scope.
- New `mogwai-server` tests, each `brokkr test -p mogwai-server <NAME>`:
  - `warmup_window_on_tape_returns_bars` - `[sim_now-1h, sim_now]` against a
    boot-derived origin yields non-empty trades (the #13 regression pin).
  - `trades_before_data_origin_refuses` - `start < data_origin` -> `422` naming
    the floor, not an empty `200`.
  - `live_seek_starts_at_sim_now_identity` - first live tick `ts_event`
    ~`sim_now`, NOT `data_origin` (no 24h backfill replay).
  - `live_seek_is_continuous_with_history` - the live stream's first tick
    continues the same realization the warmup window ends on (price level
    contiguous; same tape).
- Re-baseline `generated_history_is_replayable_and_cursorable`
  (`main.rs:1169-1185`) to pass an explicit `data_origin` and assert the same
  page-2-resumes-at-cursor continuity (still holds: both pages seek one shared
  origin).
- End-to-end: a `mogwai.toml` whose clock is coherent by construction (identity
  default now is), then `brokkr run -p mogwai-server -- serve` +
  `python3 scripts/smoke.py` - smoke stays green (it drives live WS + control
  plane; the live-seek change must not regress it).

### Landing 3 - Publish the affordance (wire) + adapter consumption

`ServerClock` in `mogwai-protocol`; `/clock` returns it; adapter `fetch_clock`
deserializes it; adapter pre-checks warmup against `data_origin_ns` and refuses
loudly; adapter live subscribe sends `start_ts = None`.

Gates:

- `brokkr test -p mogwai-protocol <NAME>` - `server_clock_serde_round_trip`
  proving both ends serialize identical bytes (wire-protocol gate, pin 5).
- `brokkr check`.
- `mogwai-adapter` tests: `request_bars_off_tape_window_errors_loudly` (a
  pre-origin window surfaces an error, not a silent drop); extend the existing
  `request_bar_aggregation_closes_on_window_and_drops_partial`
  (`client.rs:3149`) to confirm an on-tape window still aggregates.
- Cross-repo confirmation (NOT a mogwai gate, recorded for the operator): a
  `ba forward mog_smoke.pine --account mog_acct.toml` against a coherent mogwai
  now warms up and trades. This is the real-world #13 close; it cannot be a
  mogwai CI gate because broadarrow is a separate repo.

### Landing 4 - Checkpointed seek (PROCEEDS per Landing 1)

Landing 1's reading selected it (`C_F 244k > C_B 190k`). Per-symbol checkpoint
index: snapshot
`GeneratedSource` (clone of `rng`/`vol`/`acd`/`bounce`/`clock_ns` - all already
`Clone`) every `K` ticks (`K` from Landing 1); seek = binary-search the nearest
checkpoint `<= target`, restore, replay `O(K)`. The cap `C` then bounds only the
per-checkpoint replay, decoupling per-request cost from session length and
lifting the uptime ceiling. The realization is preserved byte-for-byte.

Gates:

- The fingerprint golden tests UNCHANGED - `brokkr check` must keep
  `golden_sequence` (exact `ts_event` bytes, `generated.rs:957-992`) and the ACF
  band tests green. Byte-identical output IS the correctness gate: a checkpoint
  restore that perturbs any draw is wrong.
- Re-run the Landing 1 measurement: seek cost is now flat in `K`, independent of
  distance from origin.
- `brokkr run -p mogwai-server -- serve` + `python3 scripts/smoke.py`.

## Stopping rule (out of scope, named not deferred)

- **broadarrow bridge changes** (`../broadarrow`, separate repo): the
  count-based warmup-completion guard replacing `actor.rs:273`'s
  `bars.first()`, and the honest "did not arrive" message naming
  `venue now / first available trade`. mogwai's job ends at PUBLISHING
  `data_origin_ns` (Landing 3) so broadarrow can build both. Tracked in the
  broadarrow column of `FINDINGS.md`.
- **#12** (accelerated `start was > now` warmup-window miscompute): a
  broadarrow-side clock-init ordering bug with a different root; it surfaces as
  "no bars" too but the published origin only helps its window math, it does not
  fix it. Explicitly separate.
- **O(1) block-addressable generator** (the unbounded-past rewrite): deferred to
  its own item; it forces a fingerprint golden re-baseline that #13 does not
  need.
- **Per-symbol horizon / session-aware availability** (the #4 coupling): single
  global horizon now. Per-symbol availability belongs on `/instruments` once
  cadence/session profiles diverge enough to matter; until then a single
  `data_origin_ns` is authoritative.
