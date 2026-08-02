# Technical implementation spec: one seed, one fixed origin, one reported path

Written against `reference/technical-implementation-spec.md`, which is the
contract this document is judged by. It descends from
`notes/problem-seeds-and-paths.md` (RESOLVED), which is the sole source of the
decisions applied here; where this spec appears to decide something, it is
applying that document's rulings to concrete artifacts, and where the two
disagree the problem statement wins and this spec is wrong. The `notes/todo.md`
entry naming the item is the PROBLEM STATEMENTS bullet, the
`notes/problem-seeds-and-paths.md` sub-bullet.

This is a FULL REWRITE of how a run's realization is chosen, anchored and
reported. It is not additive: the wall-derived tape origin, the identity clock,
the per-symbol seed derivation and the origin plumbing that threads
`data_origin` through half the server are all deleted, and the code that
replaces them is smaller than the code removed.

## Summary of the target

A run draws ONE 64-bit seed at launch, or takes it from config. Everything
random in the run derives from it by domain-separated derivation: the tape
generator's stream and the fill band's stream, and nothing else exists. The
tape's origin is the FIXED constant `TAPE_ORIGIN_NS = 0`; the run proper begins
at `warmup_ns` on the same axis, so the served tape is a pure function of
(seed, config) for a given build and fingerprint, with no wall-clock input
anywhere in its identity. The readiness record reports the run seed and nothing
new besides. A single `TAPE_PROTOCOL_VERSION` constant, surfaced in `mogwai
--version` and therefore in the record's `version_string`, tells an operator
whether two runs' tapes are comparable at all, and `AGENTS.md` binds the
obligation to bump it.

What does NOT change: within-run determinism (it strengthens - one fixed seed
per run is exactly what the checkpoint chain wants), the checkpointed seek, the
warmup materialization contract, the wire protocol between venue and adapter,
and the fill model.

## Survey of the ground

### Sibling reconciliation

Required by point 8 of the contract, against the problem statements authored as
a batch.

- `notes/problem-instrument-model.md` owns the CONTENT of the instrument and
  its decision 10 owns fingerprint identity (version, corpus, era, fit date).
  This spec therefore does NOT give the fingerprint an identity and does not
  add a model selector; see the stopping rule.
- `notes/problem-trade-cadence.md` owns the tape's cadence and the currently
  invalidated duration ACF anchor. This spec changes WHICH realization is drawn
  and where it starts, never the process that draws it. No generator constant,
  ACD parameter, GARCH parameter or fingerprint value is touched.
- `notes/problem-instrument-profiles.md` raises gate scoping per model. Out of
  scope here for the same reason as the fingerprint identity.
- The landed server-lifecycle work already reduced the venue to one process,
  one run, one instrument, no subscriptions. That is what makes "one seed per
  run" identical to "one seed per instance" and what makes the process-global
  checkpoint chain legal. This spec depends on that having landed and does not
  reopen it.

### What exists today, by artifact

`crates/mogwai-server/src/source.rs` carries `seed_for(symbol)`, an FNV-1a-64
fold over the symbol, used to seed BOTH the generator (`generator()`) and the
fill band (`Run::new`). It also carries the process-global `INDEX` checkpoint
chain, the `BOOT_REGIME` global, and the `data_origin` parameter threaded
through `index`, `build_live_source`, `build_history_source`,
`materialize_warmup` and `last_trade_at_or_before`, with a long doc comment
justifying why the boot estimate is always at or below every later origin.

`crates/mogwai-server/src/main.rs` builds a PRE-WARM clock from wall time to
name the warmup window, materializes warmup, then builds a SECOND clock and
re-derives `data_origin_ns = run_start_ns - warmup_ns` from the post-warmup
`run_start_ns`, precisely because the identity clock slides by the wall cost of
warmup. It writes the readiness record with
`SeedReport::PerSymbolFnv(vec![(symbol, seed_for(symbol))])`.

`crates/mogwai-server/src/config.rs` carries `sim_epoch_ns` (0 keeps the
identity wall clock), `wall_anchor_ns` (0 anchors at boot), `speed`,
`gap_cap_ms` and `warmup_ns`, plus `build_sim_clock` with the validation web
that ties them together (a wall anchor requires an epoch; a non-unit, non-zero
speed requires an epoch; a pinned anchor may not be in the future).

`crates/mogwai-protocol/src/clock.rs` carries `SimClock` with
`sim_epoch_ns`/`wall_anchor_ns`/`speed`, `identity()`, `is_identity()`, and
`ServerClock` (which reports `data_origin_ns` and `warmup_ns` on `/clock`).

`crates/mogwai-protocol/src/ready.rs` carries `ReadyRecord` at `VERSION = 4`
with the `SeedReport` enum whose single variant is `PerSymbolFnv`, plus a doc
comment stating that a run seed is deliberately not invented yet.

`crates/mogwai-server/src/tape.rs` paces by sim deadline when
`!sim.is_identity()`, and by chained relative gaps capped at `gap_cap_ms` when
it is identity. `gap_cap_ms` has no other Rust CALL SITE, which is not the same
as having no other consumer: `crates/mogwai-server/tests/configs/paced.toml`
sets `gap_cap_ms = 200` as the divergence gate's venue and depends on the
behaviour, see O11. `tape.rs` also reads `zero_speed_stall_ms`, which this spec
does NOT touch: it bounds how long a stalled client may hold the tape, has
nothing to do with the clock's epoch or with pacing cadence, and survives O4
unchanged.

`crates/mogwai-adapter/src/client/shared.rs` carries `ensure_on_tape(start,
data_origin)`, which SKIPS its check when `data_origin == 0` - zero is its
"floor unknown" sentinel for a failed `/clock` fetch. `client/data.rs` calls it
on both warmup paths. See O12.

`crates/mogwai-server/src/fills.rs` unit tests carry their own `TEST_ORIGIN =
1_700_438_400_000_000_000` and pass it as `data_origin` to
`source::build_history_source` and `scan_triggers`; `fill_golden.rs` carries the
same instant as `ORIGIN`. Both reach the tape through the shipped functions and
therefore through the process-global chain. See O13.

`scripts/smoke.py` hardcodes `READY_VERSION = 4` and raises on mismatch, and it
SPAWNS ITS OWN venue rather than attaching to one already running. It is an
artifact of this spec, not just a gate. See O14.

`crates/mogwai-server/Cargo.toml` enables only clap's `derive` feature. See O8.

`reference/performance.md` names `seed_for` durably, in the
`build_history_source` cost breakdown.

`crates/mogwai-server/src/run.rs` computes `fill_seed = seed_for(symbol)` and
`data_origin_ns` from `started_ns - warmup_ns`, and exposes
`Run::data_origin_ns()`; `http.rs`, `fills.rs` and `sweeper.rs` all read it and
pass it back down into `source`.

`crates/mogwai-engine` roots the fill band at `EngineConfig::fill_seed` and
draws each trigger from `draw_key(fill_seed, order, price, band_draw)`, an FNV
fold feeding a fresh `ChaCha8Rng` per draw. The band's stream is therefore
already stateless and already independent of the generator's stream; the
derivation discipline decision 2 demands is satisfied by construction and the
only thing missing is that both roots come from the same symbol hash.

`crates/mogwai-data/src/generated/source.rs` seeds one `ChaCha12Rng` from the
passed seed. `generated/session.rs` derives UTC hour and day-of-week straight
from the nanosecond clock, Sun=0 via `(days + 4) % 7`, which is well defined at
0 (1970-01-01 was a Thursday).

`crates/mogwai-data/src/generated/tests.rs` has `realism()` at seed 42 and
`default_symbol_tape_dwell_is_bounded`, the latter duplicating the FNV fold to
assert the dwell bound on "the tape broadarrow actually consumes". That premise
dies with `seed_for`.

`crates/mogwai-server/src/fill_golden.rs` certifies fill timing against
`tests/golden/fill_distribution.json`, using its own fixed `ORIGIN` and
`seed_for(SYMBOL)` for the engine's fill seed, and reaching the tape through
`source::build_history_source`, i.e. through the process-global chain.

`crates/mogwai-server/src/gen.rs` defaults `--seed` to `seed_for(--symbol)`.

Configs that pin clock keys: `mogwai.toml`, `scripts/smoke-accelerated.toml`,
`scripts/smoke-stop.toml`, `scripts/smoke-admission.toml`,
`scripts/smoke-heartbeat.toml`, `scripts/smoke-band-swept.toml`,
`crates/mogwai-server/tests/configs/accelerated.toml`,
`crates/mogwai-server/tests/configs/band.toml`,
`crates/mogwai-server/tests/configs/paced.toml`.

Docs that assert today's shape: `reference/config.md`, `reference/clock.md`,
`reference/cli.md`, `reference/architecture.md`, `reference/performance.md`.

### Load-bearing behaviour that must survive the rip

1. Warmup is MATERIALIZED before the readiness line, and every instant in
   `[data_origin, run_start]` is answerable afterwards.
2. `data_origin_ns == run_start_ns - warmup_ns` (asserted by `scripts/smoke.py`
   and by `run.rs`'s unit test).
3. The declared run duration is measured from the post-warmup `started_ns`, so
   a large warmup cannot eat a run.
4. `speed = 0.0` is an unpaced firehose whose sim clock still ADVANCES, or
   nothing that reads sim-now (the deadline task, the fill sweeper, the
   trailing-volatility window) works.
5. The engine's trigger draw stays a pure function of (fill seed, order
   identity), so a client cannot move the tape by placing orders.

## Obstacles resolved inline

**O1. `0 - warmup_ns` does not exist in `u64`.** Decision 1 says the tape
begins at `0 - warmup_ns` and reaches 0 when the run proper begins. Timestamps
are `u64` unix nanoseconds end to end, on the wire and in nautilus
`UnixNanos`; making them signed is a far larger rewrite than this item and buys
nothing, since the axis is affine and its zero is a label. The ruling is
therefore realized TRANSLATED, not negated: `TAPE_ORIGIN_NS = 0` is the tape's
first instant and the run proper begins at `TAPE_ORIGIN_NS + warmup_ns`. Every
property the ruling asks for holds exactly - a fixed epoch, no wall-clock input
to the tape's identity, the same origin for every run with the same config -
and the only thing that differs from the literal wording is which end of the
warmup span carries the zero.

**O2. The identity clock cannot survive a fixed epoch.** Today `sim_epoch_ns =
0` means "sim time IS wall time", which is exactly the wall-derived origin
decision 1 deletes. So the identity clock goes as a RUN clock: every run now
builds `SimClock { sim_epoch_ns: TAPE_ORIGIN_NS + warmup_ns, wall_anchor_ns:
<boot wall instant, after warmup>, speed }`. `SimClock::identity()` itself
stays, because the adapter uses it as the un-accelerated default for reconnect
policy and HTTP quotas, where it means "speed 1, no simulation" rather than "no
epoch". Note what is NOT removed: `SimClock` still holds a wall anchor, because
pacing a tape against wall time requires one. What is removed is any wall input
to the tape's IDENTITY - the anchor now only decides when a tick is delivered,
never which tick it is.

**O3. `gap_cap_ms` loses its only consumer.** It is read solely by the identity
pacing branch, which O2 deletes; in accelerated mode it never applied. Keeping
it would mean capping a wall sleep while sim time advances by the full gap,
which puts the tape AHEAD of the clock that dates it. It is therefore deleted,
config key and all. Consequence, flagged rather than buried: a `speed = 1.0`
run now really waits out the fitted arrival process, whose dwells run to tens
of seconds on the default BTCUSDT profile. That is the honest simulation, and
the knob that answers it is `speed`.

**O4. `speed = 0.0` must not freeze the clock.** With the identity clock gone,
naively passing `speed = 0.0` into `SimClock` freezes sim time at the epoch,
which would stall the deadline task, the fill sweeper and the volatility
window (load-bearing item 4). Resolved: `speed = 0.0` means UNPACED DELIVERY,
not a stopped clock. The run clock is built with `speed.max(...)` replaced by
an explicit branch - `if cfg.speed == 0.0 { 1.0 } else { cfg.speed }` - so sim
time advances at wall rate from the fixed epoch, exactly as the identity clock
made it do today, while `tape::pace` keeps branching on `cfg.speed == 0.0` to
skip pacing entirely. `/clock` therefore advertises speed 1.0 for a firehose
run, which is what it advertises today.

**O5. Near-1970 timestamps reaching nautilus.** The served run begins at
`warmup_ns` (86400e9 by default, i.e. 1970-01-02), and only the warmup floor
touches 0 exactly. Nautilus has one zero-sentinel on `UnixNanos`
(`is_zero()`), and no consumer in `research/nautilus_trader` uses it as an
"unset" marker for event timestamps; the `is_zero()` call sites are all
`Quantity`/`Money`. Bar aggregation, timers and the clock's alert validation
all compare against the same axis, so they are translation-invariant. This is
still the item with the least test coverage in the workspace, so the four
socket-backed adapter binaries are named as an explicit gate on landing L2
rather than assumed green. Note the two access paths, per `AGENTS.md`: the
`is_zero()` survey above is READ from `research/nautilus_trader`, while the gate
that actually verifies it COMPILES against the sibling `../nautilus_trader`
checkout. The reading is evidence; the socket gate is the proof.

**O6. Where randomness comes from.** `mogwai-server` does not currently depend
on `rand`. It gains `rand.workspace = true` and draws the run seed with
`rand::random::<u64>() >> 1` exactly once, at boot, before anything else reads a
seed. No other nondeterminism is introduced: this is the only call. The shift is
O15's: a seed that is reported but cannot be written back into a TOML file is
not a reproduction route.

**O7. Reproducing a path needs an ergonomic route.** Decision 2 says the seed
is "overridable in config", so the override is the `seed` key in the config
file and there is deliberately NO `--seed` CLI flag. `--duration` exists as a
flag because a launcher varies it per run without owning a file; a seed
override is the reproduction case, which is a deliberate, written-down act.
Adding a flag later is additive and cheap; adding it now would be a second way
to say one thing.

**O8. `TAPE_PROTOCOL_VERSION` cannot reach `--version` through the build
script.** `MOGWAI_LONG_VERSION` is composed in `crates/mogwai-server/build.rs`,
which runs before `mogwai-data` is compiled and cannot read a Rust constant
from it. Resolved by composing at runtime: `fn long_version() -> String`
formats `env!("MOGWAI_LONG_VERSION")` with
`mogwai_data::TAPE_PROTOCOL_VERSION`, and clap's derive takes an expression
(`#[command(version = long_version(), ...)]`), which it evaluates when the
command is built. Clap accepts an owned `String` there only under its `string`
feature, and `crates/mogwai-server/Cargo.toml` enables `derive` alone, so that
manifest gains `features = ["derive", "string"]`. Without it this design does
not compile, and the alternative - leaking a `&'static str` to dodge the feature
- is a worse trade than one feature flag. The same string is what the readiness
record already reports
as `version_string`, so the record gains the tape version without gaining a
field, which is exactly what decision 5 asked for.

**O9. The fill-timing golden moves.** Changing the engine's fill seed from
`seed_for("BTCUSDT")` to a named certification seed changes every drawn trigger
and therefore every cell of `tests/golden/fill_distribution.json`. Pinning the
certification seed to the old FNV value would avoid the churn at the cost of an
unexplainable magic number descending from a deleted function. Resolved: the
harness takes `GOLDEN_RUN_SEED = 42`, matching the realism gate's seed, and the
artifact is re-blessed in landing L1 by the documented procedure (delete the
file, run the test, inspect the diff, run again). The re-bless expectation:
every `latency_ns`, `passes`, `filled`, `censored`, `buy_filled` and
`sell_filled` value may move; `schema`, `symbol`, `data_origin_ns` (frozen in
L1 only - O13 moves it in L2, in a second, separately caused re-bless),
`sweep_interval_ns`, `horizon_ns`, `orders_per_offset`, `accept_stride_ns`,
`band_vol_mult`, `offset_ticks` and `samples` may NOT, and `assert_shape` still
holds unchanged. A diff that moves a field in the second list is a bug, not a
re-bless.

**O10. `default_symbol_tape_dwell_is_bounded` loses its premise.** It exists
because the served walk used a symbol-derived seed that no other test covered.
With the seed drawn per run there is no privileged served realization; the
honest successor asserts the dwell bound over a SAMPLE of run seeds. Resolved
in L1 by replacing it with `run_seeded_tape_dwell_is_bounded` (one derived
seed, in the default gate) plus `#[ignore]`d
`dwell_is_bounded_across_run_seeds` (eight derived seeds), so the fleet claim
"every path this venue can draw respects the dwell bound" has an instrument,
and `brokkr check` does not grow eight 2M-tick walks.

**O11. `paced.toml` loses the cadence the divergence gate depends on.** O3
deletes `gap_cap_ms` and states the consequence for operators, but the first
casualty is inside this workspace's own harness.
`crates/mogwai-server/tests/configs/paced.toml` is the divergence gate's venue -
`tests/common/mod.rs` hands it to the integration tests as the default paced
venue - and it exists precisely to keep at most one frame in flight so a
blackout window reads as silence rather than as drained backlog. It runs `speed
= 1.0` with `gap_cap_ms = 200`. Delete the cap and that gate waits out real
fitted dwells of tens of seconds, which does not merely slow the suite: a
divergence armed for a window that contains no tick at all is untestable.
Resolved by SUBSTITUTION rather than by deletion: `paced.toml` drops
`gap_cap_ms` and takes `speed = 60.0`, which compresses the same fitted
inter-arrival process into a cadence bounded by the same order of magnitude the
cap gave it, while keeping the property the file's header actually asks for
(pacing, one frame in flight, silence observable). The file's comment is
rewritten to say that, and any divergence test that hardcoded a wall-clock
window derived from the 200 ms cap is retuned in the same landing. This lands in
L2 with the rest of the `gap_cap_ms` deletion, and it is a named gate on L2 that
the divergence integration tests stay green.

**O12. The adapter's on-tape guard is silently disabled by a zero origin.**
`ensure_on_tape` skips its check when `data_origin == 0`, because zero is its
sentinel for "the `/clock` fetch failed and the client fell back to identity".
`TAPE_ORIGIN_NS = 0` makes that sentinel the permanent, correct value for every
run, so the guard would never fire again - and the `http.rs` bullet's claim that
`/clock` keeps reporting `data_origin_ns` "because the adapter's warmup guard
reads it" would be false the moment it landed. This is a real change and it is
budgeted here rather than waved past: the sentinel is replaced by an explicit
option. `data_origin_ns` on the client becomes `Option<u64>`, `None` meaning
"floor unknown, the `/clock` fetch failed", `Some(0)` meaning "the floor is
zero"; `ensure_on_tape(start: Option<UnixNanos>, data_origin: Option<u64>)`
checks whenever the floor is `Some` and skips only on `None`. The wire is
UNCHANGED - `/clock` still carries a plain `data_origin_ns: u64` - because the
distinction is client-side state (did the fetch succeed) and was only ever
encoded in-band by accident. Both `client/data.rs` call sites and the identity
fallback path move with it. New instrument in L2:
`an_unknown_floor_skips_the_guard_but_a_zero_floor_enforces_it`, in
`mogwai-adapter`, asserting that `ensure_on_tape(Some(5), None)` is `Ok` and
`ensure_on_tape` with a `Some(10)` floor and a start of 5 is an error - the
exact pair the old sentinel could not distinguish.

**O13. The fill harnesses' 2023 origin cannot survive a chain rooted at zero.**
`fill_golden.rs` (`ORIGIN`) and the `fills.rs` unit tests (`TEST_ORIGIN`) both
use `1_700_438_400_000_000_000` and today pass it down as `data_origin`, so the
checkpoint chain's zero IS that instant and the seek is free. Once `index()`
loses the parameter and roots at `TAPE_ORIGIN_NS = 0`, reaching 1.7e18 means
generating roughly 53 years of tape from zero; `MAX_EXTEND_TICKS = 1 << 30`
trips long before that, so these tests do not slow down, they fail. Setting
`BootTape` does not help - it carries the seed and the regime, not the origin.
The earlier claim that `fill_golden.rs`'s "own `ORIGIN` constant stays" is
therefore WITHDRAWN and replaced: both harnesses move their schedules onto the
fixed axis. `fill_golden.rs` takes `const ORIGIN: u64 = TAPE_ORIGIN_NS +
GOLDEN_WARMUP_NS` where `GOLDEN_WARMUP_NS = 86_400_000_000_000`, i.e. it
certifies against a one-day-in tape rather than a 2023 one, and `fills.rs`'s
`TEST_ORIGIN` becomes the same expression. Consequently O9's may-NOT-move list
is CORRECTED: `data_origin_ns` in `tests/golden/fill_distribution.json` DOES
move, from `1700438400000000000` to `86400000000000`, and it moves in L2, not in
L1. The golden is therefore re-blessed TWICE across this work - once in L1 for
the seed change (every timing cell, `data_origin_ns` frozen) and once in L2 for
the origin change (`data_origin_ns` plus every timing cell, since a different
stretch of the same seeded walk is being sampled). Two re-blesses with two
stated causes are honest; one re-bless hiding two causes is not. The frozen
list in both cases remains `schema`, `symbol`, `sweep_interval_ns`,
`horizon_ns`, `orders_per_offset`, `accept_stride_ns`, `band_vol_mult`,
`offset_ticks` and `samples`. Every `fills.rs` call site that passes an origin
argument is inventoried and updated in L2 as part of the `data_origin` parameter
removal; none of them assert on the origin's value, they only need a floor at or
below their probes.

**O14. `scripts/smoke.py` is an artifact, and the gate lines misuse it.** Two
distinct defects. First, it hardcodes `READY_VERSION = 4` and raises on
mismatch, so the L1 bump to 5 breaks it - it is added to the L1 artifact list
and the constant goes to 5 there, not in L4. Second, the gate lines that say
"run `brokkr run mogwai -- serve ...` in one terminal, `python3
scripts/smoke.py` in another" describe something the script does not do: it
spawns its OWN venue with its own `--ready-fd` pipe, so the manually launched
server is never exercised and, on a fixed address, would collide with the one
the script spawns. Every gate in this spec that mentions smoke.py is rewritten
to invoke the script alone with its mode and config, e.g. `python3
scripts/smoke.py accelerated`, and no separate `brokkr run` line accompanies it.
Its `data_origin_ns == run_start_ns - warmup_ns` assertion still holds after L2
with both sides constant, which is worth keeping precisely because it is now a
statement about a constant rather than about a subtraction.

**O15. `u64` seeds do not round-trip through TOML.** The run seed is drawn from
the full `u64` range but configured as `seed: Option<u64>` in a TOML file, and
TOML integers are `i64`. Roughly half of all drawn - and reported - seeds
therefore cannot be written back into a config file, which breaks the one
ergonomic route O7 deliberately chose as the ONLY route to reproduction. That is
not an edge case, it is a coin flip on every reproduction attempt. Resolved by
narrowing the draw, not by complicating the config: the launch draw is
`rand::random::<u64>() >> 1`, so every reported seed is `<= i64::MAX` and is a
literal an operator can paste into TOML. Sixty-three bits of seed space is
indistinguishable from sixty-four for every purpose this spec has - the
collision argument in the stopping rule is 2^-63 instead of 2^-64 - and the
alternative (a quoted decimal string with custom deserialization) makes the
config key ugly to satisfy a range nobody needs. `seed` stays `Option<u64>` in
the struct, so a hand-written config may still name any value up to `i64::MAX`;
values above it are refused by the TOML parser with its own error, which is
correct. New instrument in L1, in `config.rs`:
`a_configured_seed_at_the_signed_maximum_round_trips` - serialize a config with
`seed = i64::MAX as u64`, parse it back, assert equality; and assert that the
launch draw never exceeds `i64::MAX` over a sample.

**O16. The boot global makes the tape seed order-dependent.** Replacing
`BOOT_REGIME` with `static BOOT: OnceLock<BootTape>` moves the TAPE SEED into
process-global once-set state. Today the golden's seed comes from `seed_for`,
which is a pure function and therefore order-independent; afterwards, whichever
test in the `mogwai-server` lib test binary touches the chain first fixes the
seed for every later one, and `set_boot_for_test` becomes a silent no-op.
`--test-threads=1` orders the tests but does not choose the order. Resolved by
making the collision LOUD rather than by hoping: `set_boot_for_test` asserts
that either `BOOT` is unset or the already-set value equals the one being
installed, and panics naming both otherwise. Then `fill_golden.rs` and the
`fills.rs` unit tests all install the SAME `BootTape { seeds:
RunSeeds::from_run_seed(GOLDEN_RUN_SEED), regime: None }` through one shared
test helper, so the assertion holds by construction and any future test that
wants a different seed fails immediately and by name instead of silently reading
someone else's. The same helper is what the golden's paragraph refers to.
`set_boot_for_test` is named in the `source.rs` artifact list below, not only in
prose.

## The target, as concrete artifacts

### `mogwai-protocol`

New `src/seeds.rs`, re-exported from `lib.rs`:

```rust
/// Every random stream in one run, derived from the run's single seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSeeds {
    /// The seed as drawn or configured. The value reported and reproduced.
    pub run: u64,
    /// Root of the tape generator's stream.
    pub tape: u64,
    /// Root of the fill band's draw stream. Separate from `tape` so the number
    /// of orders a client places cannot move the tape.
    pub fill: u64,
}

impl RunSeeds {
    #[must_use]
    pub const fn from_run_seed(run: u64) -> Self;
}
```

Derivation is splitmix64 over `run ^ DOMAIN`. Domain separation rather than
"tape gets the seed, fill gets seed + 1", so adjacent run seeds do not alias one
another's streams. "splitmix64" and "the ASCII of a name" are not enough to
implement twice identically - variant, constants and byte order all have to be
pinned, or two implementers produce two different, equally plausible artifacts.
So the body is the spec, verbatim:

```rust
const DOMAIN_TAPE: u64 = u64::from_le_bytes(*b"tape_gen"); // 0x6e65675f65706174
const DOMAIN_FILL: u64 = u64::from_le_bytes(*b"fill_bnd"); // 0x646e625f6c6c6966

const fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl RunSeeds {
    #[must_use]
    pub const fn from_run_seed(run: u64) -> Self {
        Self {
            run,
            tape: splitmix64(run ^ DOMAIN_TAPE),
            fill: splitmix64(run ^ DOMAIN_FILL),
        }
    }
}
```

Little-endian `from_le_bytes` is stated because it is the one free choice in
"the ASCII of `tape_gen`", and the hex value is written beside each constant so
the choice is checkable without running anything. The three `(tape, fill)` pairs the
L1 test pins are given here rather than left to the implementer, since a test
that invents its own expected values pins nothing:

| `run` | `tape` | `fill` |
|---|---|---|
| `0` | `0x31ad_0d8b_b6c2_a429` | `0xe800_e9a6_2035_1b1b` |
| `1` | `0x97cf_101c_51b0_7fa5` | `0xf019_ac7a_e519_ce08` |
| `u64::MAX` | `0xa74e_19e5_da36_6019` | `0xac14_3574_b13c_7a54` |

A diff to any of the four constants above must show up as a failure of that test
rather than as a silently different tape. `u64::MAX` is kept as a test input
even though O15 caps DRAWN seeds at `i64::MAX`, because `from_run_seed` is total
over `u64` and the derivation must be pinned across the whole domain.

`src/ready.rs`: delete `SeedReport` entirely. `ReadyRecord` gains `pub
run_seed: u64` where `seed: SeedReport` was, and `VERSION` goes 4 to 5. The
struct's doc comment stops apologizing for not having a run seed and states
what the field means: the value that, with the config, the fingerprint and
`version_string`, reproduces this path.

`src/clock.rs`: delete `is_identity()`. `identity()` and everything else stay.

### `mogwai-data`

`lib.rs` gains:

```rust
/// Identity of the tape GENERATION PROCESS, not of any one path. Two runs are
/// comparable only if their venues report the same value here: a bump says
/// "new runs are not equivalent to your old ones", and nothing can detect a
/// generation change that failed to bump it. `AGENTS.md` carries the
/// obligation - any change affecting tape determinism bumps this.
pub const TAPE_PROTOCOL_VERSION: u32 = 1;
```

`generated/tests.rs`: `default_symbol_tape_dwell_is_bounded` is replaced per
O10. Both successors build their scalars the same way it did (fingerprint
medians overlaid with the default instrument's increment and precision) and
seed with `RunSeeds::from_run_seed(n).tape`, which removes the duplicated FNV
fold and the stale comment about the server keying on the symbol.

### `mogwai-server`

`config.rs`:

- DELETE `sim_epoch_ns`, `wall_anchor_ns`, `gap_cap_ms` and every validation
  branch that mentions them.
- ADD `pub(crate) seed: Option<u64>` (absent means drawn at launch). Values
  above `i64::MAX` are unrepresentable in TOML and are refused by the parser;
  the launch draw never produces one, per O15.
- `build_sim_clock(cfg, boot_wall_ns)` is replaced by:

```rust
/// The run's clock. The epoch is FIXED by config alone - the tape's first
/// instant is `TAPE_ORIGIN_NS` and the run proper begins one warmup later - so
/// the wall anchor decides only WHEN a tick is delivered, never which tick it
/// is. `speed == 0.0` is unpaced delivery, not a stopped clock: the axis still
/// advances at wall rate so the deadline, the sweeper and the volatility
/// window keep working.
pub(crate) fn build_run_clock(cfg: &Config, boot_wall_ns: u64) -> anyhow::Result<SimClock>;
```

  It refuses a non-finite or negative speed and nothing else; the three
  epoch/anchor cross-checks are gone because their subjects are gone.

`source.rs`:

- DELETE `seed_for`.
- ADD `pub(crate) const TAPE_ORIGIN_NS: u64 = 0;` with the doc comment stating
  that this is the fixed epoch of decision 1 and that the run proper begins at
  `TAPE_ORIGIN_NS + warmup_ns`.
- Replace `BOOT_REGIME` with one boot global,
  `static BOOT: OnceLock<BootTape>`, where `BootTape { seeds: RunSeeds, regime:
  Option<MarketRegime> }`, set once by `materialize_warmup` and read by
  `generator`. One global instead of two, because they are set at the same
  instant by the same caller and read by the same function.
- `generator(profile)` loses its `data_origin` and `regime` arguments and reads
  `BOOT`; it passes `seeds.tape` and `TAPE_ORIGIN_NS` to `GeneratedSource`.
- `index(symbol, profiles)`, `build_live_source(symbol, profiles, sim_now)`,
  `build_history_source(symbol, start, profiles)`,
  `last_trade_at_or_before(symbol, ts, profiles)` all lose their `data_origin`
  parameter. `build_history_source`'s `start.unwrap_or(data_origin)` becomes
  `start.unwrap_or(TAPE_ORIGIN_NS)`.
- `materialize_warmup(symbol, profiles, boot: BootTape, run_start_ns)` sets the
  global and generates as it does today. The long doc comment about the boot
  origin estimate being at or below every later origin is DELETED, not
  reworded: there is one origin now and it is a constant.
- ADD `#[cfg(test)] pub(crate) fn set_boot_for_test(boot: BootTape)`, which
  installs `BOOT` if unset and otherwise ASSERTS the already-installed value
  equals `boot`, panicking with both values named. Per O16 this is what keeps
  the tape seed from becoming silently order-dependent across the lib test
  binary; every in-crate test that reaches the chain goes through it, with the
  single shared `BootTape` O16 names.

`run.rs`: `Run::new` takes `seeds: RunSeeds` in place of the symbol-derived
fill seed and stores `pub(crate) seeds: RunSeeds` (the engine gets
`seeds.fill`); it drops the `gap_cap_ms` argument. `data_origin_ns()` returns
`TAPE_ORIGIN_NS`; `started_ns` remains a field because the deadline and the
completion event measure from it, and its value is now `TAPE_ORIGIN_NS +
warmup_ns` by construction. The unit test
`the_history_floor_is_derived_from_the_warmup_span` is rewritten as
`the_history_floor_is_the_fixed_tape_origin`.

`main.rs`: the pre-warm clock, `prewarm_start_ns`, `prewarm_origin_ns` and the
second clock build all collapse into:

```rust
let seeds = RunSeeds::from_run_seed(cfg.seed.unwrap_or_else(|| rand::random::<u64>() >> 1));
let run_start_ns = source::TAPE_ORIGIN_NS.saturating_add(cfg.warmup_ns);
tracing::info!(run_seed = seeds.run, tape_seed = seeds.tape, fill_seed = seeds.fill, "run seeds fixed");
let checkpoints = spawn_blocking(materialize_warmup(.., BootTape { seeds, regime }, run_start_ns)).await??;
let sim = config::build_run_clock(&cfg, now_ns())?;
```

The clock is still built AFTER warmup (load-bearing item 3 is unchanged: the
anchor must not include the wall cost of generation), but there is no longer a
first clock whose only job was to name the window. `ReadyRecord` is written
with `run_seed: seeds.run` and `data_origin_ns: source::TAPE_ORIGIN_NS`, and
`version_string: long_version()`.

`tape.rs`: `TapeSpawn` loses `gap_cap_ms`; `pace` becomes two branches -
`speed == 0.0` parks on headroom, everything else sleeps to
`sim.wall_ns(ts)`. The chained-deadline machinery (`previous`, `deadline`,
`wall_anchor` threading) that existed only for identity pacing goes with it;
`sleep_until_wall_cancellable` stays.

`http.rs`, `fills.rs`, `sweeper.rs`: every `state.data_origin_ns()` read and
every `data_origin` argument disappears. `AppState::data_origin_ns` is deleted.
`fills::read_market`'s window floor becomes
`ts.saturating_sub(VOL_WINDOW_NS).max(source::TAPE_ORIGIN_NS)`. The `/trades`
off-tape refusal keeps its message and compares against `TAPE_ORIGIN_NS`; the
`/clock` envelope keeps reporting `data_origin_ns` (now constant) because the
adapter's warmup guard reads it - which is only true once O12 replaces that
guard's zero sentinel, so the two changes land together or the guard dies
silently. `fills.rs`'s unit tests move their `TEST_ORIGIN` onto the fixed axis
per O13.

`mogwai-adapter/src/client/shared.rs` and `client/data.rs`: `ensure_on_tape` and
the client's stored `data_origin_ns` become `Option<u64>` per O12. No wire
change.

`gen.rs`: `--seed` defaults to `DEFAULT_GEN_SEED = 42`, a named constant with a
comment tying it to the realism gate, and its help text says so. The two unit
tests comparing against `seed_for("BTCUSDT")` compare against the constant.

`fill_golden.rs`: `GOLDEN_RUN_SEED: u64 = 42`; the engine takes
`RunSeeds::from_run_seed(GOLDEN_RUN_SEED).fill`, and the harness sets the boot
global (`source::set_boot_for_test(BootTape { seeds, regime: None })`, the
shared helper of O16) before touching the chain, since it reaches the tape
through the shipped functions. Its own `ORIGIN` constant STAYS AS A CONSTANT -
the harness still certifies fill timing against a fixed tape of its own rather
than against the served run, and still records that origin in the artifact - but
its VALUE moves in L2 from the 2023 instant to `TAPE_ORIGIN_NS +
GOLDEN_WARMUP_NS`, because a chain rooted at zero cannot reach 2023 (O13).

## Landings

Three landings, L1 to L3. Each is a coherent, fully intrusive change that leaves
the suite green at its boundary, kept or reverted on its own gates. L4 is listed
alongside them for readability but is a work item distributed across the three,
not a fourth boundary - see its own section.

### L1 - one seed per run, derived, reported

`RunSeeds` and the record change; `seed_for` deleted; the config `seed` key and
the launch draw; engine and generator rooted in the derived seeds; the golden
re-blessed; the dwell tests replaced. The origin is still wall-derived after
this landing, which is why L1 stands alone: it changes WHICH realization runs,
and every artifact pinned to the old realization moves here and only here.

What L1 therefore CANNOT claim, stated because an earlier draft of this spec
claimed it: end-to-end reproduction. While the origin is still wall-derived, two
launches with the same configured seed start at different origins, and session
modulation reads those absolute timestamps, so their served trades differ in
both timestamp and price. The seed is fixed and REPORTED in L1; it only becomes
SUFFICIENT in L2. The whole-item central test therefore lives in L2, and L1's
own reproduction evidence is the in-process one: the same derived seeds produce
the same walk when the origin is held fixed, which is what the golden and the
dwell tests already witness.

New instruments, all specified as part of this landing:

- `crates/mogwai-protocol/src/seeds.rs`:
  `derived_streams_differ_and_are_stable` - `from_run_seed(0)`,
  `from_run_seed(1)` and `from_run_seed(u64::MAX)` each yield `tape != fill`,
  adjacent run seeds yield unrelated streams, and three literal expected values
  are pinned so a derivation change is loud.
- `crates/mogwai-protocol/src/ready.rs`: the existing round-trip test updated
  to `VERSION = 5` and the flat `run_seed` field, pinning the exact bytes.
- `crates/mogwai-server/tests/lifecycle.rs`:
  `the_ready_record_reports_a_seed_that_differs_between_launches` - spawn two
  venues, assert both records carry a `run_seed` and that they differ.
  (Different-by-draw, not different-by-guarantee: the test states in a comment
  that a collision is a 2^-63 event, not a bug.)
- `crates/mogwai-server/src/config.rs`:
  `a_configured_seed_at_the_signed_maximum_round_trips`, per O15.
- `crates/mogwai-data/src/generated/tests.rs`:
  `run_seeded_tape_dwell_is_bounded` and the `#[ignore]`d
  `dwell_is_bounded_across_run_seeds`, per O10.

Also moving in L1, not in L4: `scripts/smoke.py`'s `READY_VERSION` goes 4 to 5.
It raises on mismatch, so without this the landing fails its own gate (O14).

Gates:

- `brokkr check`
- `brokkr test -p mogwai-server fill_distribution_matches_the_golden` (after
  the O9 re-bless procedure; the artifact diff is inspected against the
  may-move / may-not-move lists in O9 before the second run)
- `brokkr test -p mogwai-data dwell_is_bounded_across_run_seeds`
- `python3 scripts/smoke.py` (the script spawns its own venue; no separate
  `brokkr run` line, per O14)

### L2 - the fixed origin

`TAPE_ORIGIN_NS`, the run clock, the deletion of `sim_epoch_ns`,
`wall_anchor_ns`, `gap_cap_ms`, `is_identity`, the pre-warm clock and the
`data_origin` plumbing. Every config file listed in the survey is updated in
this landing; `crates/mogwai-server/tests/configs/accelerated.toml` and
`band.toml` keep their `speed` and `warmup_ns` and drop their epoch lines,
which does not change what they were testing (a large warmup against a fast
clock, and a one-hour warmup for the volatility estimator).
`crates/mogwai-server/tests/configs/paced.toml` takes the O11 substitution
(`gap_cap_ms` out, `speed = 60.0` in, comment rewritten) rather than merely
losing a key. Three further pieces land here because the fixed origin is what
forces them: the O12 `Option<u64>` rework of the adapter's `ensure_on_tape` and
its stored floor; the O13 move of `fill_golden.rs`'s `ORIGIN` and `fills.rs`'s
`TEST_ORIGIN` onto the fixed axis, with the second, separately caused re-bless
of `tests/golden/fill_distribution.json` in which `data_origin_ns` becomes
`86400000000000`; and the inventory of every `fills.rs` call site that passed an
origin argument.

New instruments:

- `crates/mogwai-server/tests/serving.rs`:
  `two_runs_with_the_same_configured_seed_serve_the_same_first_trades` - spawn
  two venues from a config that pins `seed`, read the first 50 trades from
  `/trades` on each, assert byte equality; then spawn a third with a different
  pinned seed and assert it differs. This is the whole item's central claim and
  nothing pins it today. It sits in L2 rather than L1 because only the fixed
  origin makes it true.
- `crates/mogwai-adapter`:
  `an_unknown_floor_skips_the_guard_but_a_zero_floor_enforces_it`, per O12.
- `crates/mogwai-server/tests/serving.rs`:
  `the_tape_origin_is_fixed_and_independent_of_launch_time` - spawn a venue,
  assert `record.data_origin_ns == 0` and `record.run_start_ns ==
  record.warmup_ns`, and that `/clock` agrees. Spawn a second and assert both
  records report identical origin, start and warmup, which is the property
  wall-derived origins could not have.
- `crates/mogwai-server/src/config.rs`:
  `a_config_naming_a_removed_clock_key_is_refused` - a TOML carrying
  `sim_epoch_ns` fails to parse with the key named, so an operator with an old
  file learns it rather than silently getting a different run. `Config`
  already carries `#[serde(default, deny_unknown_fields)]`, so this test pins
  behaviour the deletion gives for free rather than adding a mechanism - and it
  is worth pinning precisely because the deleted keys are the ones an operator
  is most likely to still have in a file.
- `crates/mogwai-server/src/tape.rs`: the pacing unit test set is reduced to
  the two surviving branches; any test that existed only to cover the capped
  identity path is deleted rather than reworded.

Gates:

- `brokkr check --gate` (mandatory here, not optional: L2 changes what
  timestamps the adapter and nautilus see, and the four socket-backed adapter
  binaries are the only coverage of that path - see O5)
- `brokkr test -p mogwai-server the_tape_origin_is_fixed_and_independent_of_launch_time`
- `brokkr test -p mogwai-server two_runs_with_the_same_configured_seed_serve_the_same_first_trades`
- `brokkr test -p mogwai-server fill_distribution_matches_the_golden` (the
  second re-bless, O13; `data_origin_ns` moves this time and every timing cell
  moves with it, because a different stretch of the same seeded walk is sampled)
- the divergence integration tests over `paced.toml`, green at the O11
  substituted speed and without wall-clock windows tuned to the deleted cap
- `python3 scripts/smoke.py accelerated` (the script spawns its own venue, per
  O14; it asserts `data_origin_ns == run_start_ns - warmup_ns`, which must still
  hold with both sides now constant)

### L3 - the tape protocol version and its obligation

`TAPE_PROTOCOL_VERSION`, `long_version()`, the clap wiring (including the
`string` feature the manifest must gain, per O8), and the `AGENTS.md`
rule. The rule is the deliverable, not a doc comment: a new subsection under
the project's Rules stating, by name, that any change to the tape generation
path - a generator constant, an ACD or GARCH parameter, the committed
fingerprint, the seed derivation, the fill band's draw, the tape origin - MUST
bump `mogwai_data::TAPE_PROTOCOL_VERSION`, because nothing can detect that it
should have been bumped and was not. The constant's own doc comment points back
at `AGENTS.md`.

New instrument:

- `crates/mogwai-server/tests/lifecycle.rs`:
  `the_ready_record_names_the_tape_protocol_version` - the record's
  `version_string` contains `tape <TAPE_PROTOCOL_VERSION>`, so an operator who
  kept the record can tell whether a later build's runs are comparable.

Gates:

- `brokkr check`
- `brokkr run mogwai -- --version` (prints semver, hash, build time and the
  tape version on one line)

### L4 - the durable docs

L4 is NOT the implementer's work and must not be attempted by them. Reconciling
`reference/`, `docs/`, `AGENTS.md` and `notes/` is owned by the orchestrator and
happens after the implementation has been reviewed, before the landing commit -
so markdown still rides with the code it describes, it is simply not written by
the same hand. The implementer writes inline documentation only: doc comments on
what it adds, to the exact text this spec dictates where it dictates one.

The ONE exception, because it is a deliverable rather than documentation of one:
`AGENTS.md`'s bump obligation for `TAPE_PROTOCOL_VERSION` is L3's actual output.
A ruling that the rule must live in `AGENTS.md` makes writing it there the work,
not a description of the work. The implementer writes that.

Everything below is an INVENTORY for the reconciliation pass, naming which
landing each document's truth changes with:

- With L1: `reference/config.md` for the `seed` key; `reference/performance.md`,
  whose `build_history_source` cost breakdown names `seed_for` by name and
  must stop; `mogwai.toml`'s commented `seed` key.
- With L2: `reference/clock.md`, `reference/config.md` again for the removed
  keys, `reference/architecture.md` for the fixed origin and the
  pure-function-of-(seed, config) sentence with its limit.
- With L3: `reference/cli.md` for `--version` and the record's `run_seed`, and
  `AGENTS.md`'s bump obligation (which is L3's deliverable, not documentation
  of it).
- With whichever lands last: `notes/todo.md`'s entry pointing at this spec.

The content, unchanged from the earlier draft:
`reference/clock.md` restated for a fixed epoch (the run clock, what the wall
anchor does and does not decide, `speed = 0.0`); `reference/config.md` for the
new key set (`seed`, and the removal of `sim_epoch_ns`, `wall_anchor_ns`,
`gap_cap_ms`); `reference/cli.md` for `--version` and for the record's
`run_seed`; `reference/architecture.md` for the sentence that a run is a pure
function of (seed, config) for a given build and fingerprint, WITH the limit
stated: a new seed draws a new path from ONE fitted model, so marginalizing
over seeds reduces variance conditional on that model and is not out-of-sample
market evidence. `mogwai.toml` gains a commented `seed` key explaining that
absent means drawn per launch. `reference/performance.md` loses its `seed_for`
reference. `notes/todo.md`'s entry for this problem statement is updated to
point at this spec.

Gate: `brokkr check` (gremlins runs over the docs too), on each landing that
carries a document.

## Stopping rule and what is out of scope

The teardown stops at the venue's boundary. Explicitly NOT in this spec:

- **The second variation axis (multiple fitted models).** The problem statement
  raises it and the user wants it, but it needs the fingerprint to gain an
  identity (version, corpus, era, fit date), which
  `notes/problem-instrument-model.md` decision 10 already owns for the
  fitted-versus-declared distinction, and it needs the realism gate to assert
  per model, which `notes/problem-instrument-profiles.md` raises as gate
  scoping. That is one coherent piece of work sitting on two other problem
  statements; splitting it across this spec would decide their questions by
  accident. Named and excluded, not deferred: this spec is complete without it,
  and `TAPE_PROTOCOL_VERSION` is deliberately a version of the PROCESS, so a
  model selector lands beside it without changing it.
- **Anything the consumer does with paths.** How many paths make a claim, how
  seeds are allocated across a fleet, and how provenance is attached to a
  result are the launcher's, per the problem statement. The venue's obligation
  ends at drawing a path and reporting which it drew. In particular, nothing
  here detects a duplicate seed across two concurrent instances; that is
  asserted by construction, per decision 3, at 2^-63 per pair once O15 narrows
  the draw.
- **Restart and resume.** Closed by the user. No cursor is recorded and no
  state outlives a run.
- **The cadence, the profile set, the instrument model and the fill model.**
  Untouched by name. No generator constant moves in any landing here, which is
  also why `TAPE_PROTOCOL_VERSION` ships at 1 rather than being bumped by this
  work: the process it names is not changed by this spec, only the seed and
  origin fed into it. (The realization every existing run served does change,
  which is what the golden re-bless in L1 records.)
- **A `/run` endpoint or any wire surface for the seed.** Decision 4: the
  readiness record only.

## Consequences worth the user's attention before implementation

1. **O3, the `gap_cap_ms` deletion.** A default `speed = 1.0` run will sit
   quiet through the fitted process's real dwells. Reversible by choosing a
   speed; not reversible by a knob, because the knob is deleted.
2. **O1, the translated epoch.** The run proper begins at `warmup_ns` rather
   than at 0, because `u64` has no negative half. If the literal zero at the
   run start matters more than `u64` timestamps do, that is a different and
   much larger spec.
3. **O9 and O13, the golden re-blessed TWICE.** Every fill-timing cell in the
   committed artifact moves in L1 for the seed change, and the whole artifact
   including `data_origin_ns` moves again in L2 for the origin change. Both are
   expected and inspected, not silent, and they are kept separate so each diff
   has one cause.
4. **The session phase is now fixed for every run.** `generated/session.rs`
   derives UTC hour and weekday from the absolute nanosecond clock, so with
   origin 0 and the default 86400s warmup EVERY run warms up through Thursday
   1970-01-01 and begins its run proper on Friday 1970-01-02 00:00 UTC. The
   problem statement raised this and dismissed it on the grounds that forward
   tests always run accelerated, and this spec applies that ruling - but it is
   worth the user's eye, because it interacts with O3: the honest `speed = 1.0`
   run O3 pushes an operator toward is exactly the run that never sweeps a week
   of session phase. Accelerating is what recovers the sweep, and after O3 there
   is no other lever.
5. **O12, the adapter's guard.** A live behaviour change outside the server:
   the client's `data_origin_ns` becomes an `Option` so that a zero floor is
   enforced rather than read as "unknown". No wire change, but the adapter is a
   published-surface crate and this is where the blast radius leaves the venue.
6. **O15, 63-bit seeds.** Drawn seeds are capped at `i64::MAX` so they can be
   pasted back into a TOML file. A deliberate, stated narrowing, not an
   oversight.

## Review disposition

Two independent reviews (`notes/spec-seeds-and-paths-review-1.md`,
`notes/spec-seeds-and-paths-review-2.md`) were validated finding by finding
against the tree. Every finding either landed above or is listed here with its
reason. The two reports overlap on four findings (the L1 determinism gate, the
2023 golden origin, `smoke.py`'s `READY_VERSION`, and the missing `paced.toml` /
`performance.md` artifacts), reported once here.

Folded in: L1's reproduction test moved to L2 (both reports); the golden and
`fills.rs` origins moved onto the fixed axis, O13 (both); `smoke.py` as an
artifact and the misused smoke gate lines, O14 (both); `paced.toml`'s cadence
substitution, O11 (both); `reference/performance.md` (both); the adapter's zero
sentinel, O12 (R1); the boot global's ordering hazard, O16 (R1); the fixed
session phase, consequence 4 (R1); `zero_speed_stall_ms` named as untouched and
the `gap_cap_ms` "no other consumer" claim corrected (R1); TOML's `i64` ceiling,
O15 (R2); clap's `string` feature, O8 (R2); the derivation pinned to an exact
body with literal expected values (R2); the sibling-checkout note in O5 and the
per-landing document assignment replacing L4-as-a-landing (R2).

Rejected, with reasons:

- **Merging L1 and L2** (R2's alternative remedy for its finding 1). The two
  landings answer different questions and have different revert stories: L1
  changes which realization runs, L2 changes where the axis starts. Merging them
  would put two independent causes behind one golden diff. Moving the test,
  which both reports also offered, achieves the same correctness at lower cost.
- **A quoted-string or hexadecimal seed key with custom deserialization** (R2's
  first remedy for its finding 2). Correct about the defect, wrong about the
  fix: it makes the config key awkward forever to preserve one bit of entropy
  nobody can use. R2's own second option, narrowing the draw, is what O15 takes.
- **Making O12 a wire change** (R1's framing that the fix "is a wire-surface
  change the spec does not budget for"). The defect is real and budgeted, but
  the diagnosis overshoots: whether the `/clock` fetch succeeded is client-side
  state, so an `Option` on the client is enough and `/clock` keeps its plain
  `u64`. No protocol version moves.
- **R1's note that `rand::random::<u64>()` "checks out"** and its closing
  paragraph on fidelity to the problem statement. Confirmations, not findings;
  nothing to fold, though the `rand` line did change for O15's reasons.
