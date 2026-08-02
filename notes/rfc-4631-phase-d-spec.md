# RFC 4631 phase D - fill-path benches and golden fill distributions

## Standing references

- Written against `reference/technical-implementation-spec.md`; that document
  defines what this one must contain.
- Spawned from the `notes/todo.md` entry "RFC 4631 phase D: Criterion benches on
  the fill path, and golden-file fill distributions".
- Design facts it leans on: `reference/architecture.md` ("Fills are synthetic",
  which records the phase B refusal and pins the penetration gate as the only
  tape-dependent fill predicate) and `reference/config.md` (the
  `penetration_ticks` / `fill_sweep_interval_ms` knobs).

## Goal, restated as artifacts

Two deliverables, neither of which exists today:

1. **Criterion benches on the fill path**, runnable inside this project's
   toolchain, covering the three functions a fill actually costs: the ungated
   submit-to-fill path in `mogwai-engine`, the batch `apply_scans` gate
   application, and the per-pass tape walk that counts penetrations.
2. **A golden-file fill distribution** for a penetration-gated configuration:
   the committed, byte-exact distribution of fill latency for a fixed population
   of resting limit orders driven through the real sweep pipeline against the
   real fitted tape.

The first guards a hot path that the divergence, admission, sizing and
penetration gates have all made fatter. The second turns "the gate behaves the
same way after this change" into a file diff instead of a judgement call.

## Survey of the ground

### The fill path as it exists

`mogwai-engine/src/orders.rs`:

- `Engine::on_submit(order, ts, market_px) -> Vec<ServerMessage>` - validation,
  `RejectNextSubmit`, the penetration gate branch, `plan_fill`, the FOK
  decision, id reservation, `commit_fill`, resting-or-closing, the account
  snapshot. This is the whole default-venue fill cost: at
  `penetration_ticks == 0` every accepted submit fills here, in full,
  synchronously.
- `Engine::apply_scans(&[ScanResult], ts) -> (Vec<ServerMessage>, usize)` -
  matches each result back to a resting order by
  `(client_order_id, revision, penetration_scanned_ns)`, advances the frontier,
  and executes whatever crossed the threshold. Linear scan of `self.open` per
  result, so it is quadratic in the resting-order count for a full batch. That
  is the shape a bench has to price.
- `Engine::pending_scans() -> Vec<PendingScan>` - filter plus a sort by
  `from_ns`, run once per sweep pass under the engine lock.
- `fn through(side, limit, traded) -> bool` (private, `orders.rs:720`) - the
  strictly-through predicate on the seed path.

`mogwai-server/src/fills.rs`:

- `count_penetrations(symbol, &[PendingScan], to_ns, profiles, data_origin) ->
  Option<Walk>` - builds a history source via `source::build_history_source`,
  drains up to `SWEEP_DRAIN_BUDGET` (20,000) ticks, and counts strictly-through
  prints per scan. One walk per symbol per pass. Costs a checkpoint restore, a
  positioning seek, and a process-wide mutex acquisition inside the generator,
  which is why the sweeper runs it on `spawn_blocking` off the engine lock.
- `fn through(side, limit, traded) -> bool` (private, `fills.rs:44`) - a
  character-for-character duplicate of the engine's predicate. Its doc comment
  says outright that "both sides of the seam have to agree or an order would
  fill on arrival and then be judged not-penetrated by the sweep". Nothing
  enforces the agreement; it is two copies kept in step by hand.
- Nine unit tests already drive `count_penetrations` against the real generated
  BTCUSDT tape, including two cost gates
  (`sweep_pass_walks_only_the_new_span`, `a_pass_costs_one_walk_per_symbol_not_per_order`).

`mogwai-server/src/sweeper.rs` - the loop that ties them together: sleep the
converted interval (floored at `MIN_SWEEP_WALL`), `pending_scans` under the
lock, group by symbol, walk off-lock, `apply_scans` under the lock, deliver per
session. The harness in deliverable 2 reproduces exactly this ordering,
synchronously, minus tokio and minus delivery.

### What blocks each deliverable

**Blocker A - `mogwai-server` has no library target.** Its `Cargo.toml`
declares only `[[bin]] name = "mogwai"`. Every module (`fills`, `sweeper`,
`source`, `accounts`, ...) is `pub(crate)` inside that binary. A cargo bench or
example target in that package therefore cannot call `count_penetrations` at
all. Only a `#[cfg(test)]` module compiled into the bin crate can reach it -
which is how the nine existing `fills.rs` tests do it.

**Blocker B - criterion cannot be run.** Criterion's normal home is a
`benches/*.rs` target with `harness = false`, driven by `cargo bench`. The
project rules forbid raw `cargo`, and `brokkr` has no `bench` subcommand;
`brokkr generic-hotpath --bench` is a wall-clock-plus-sidecar harness for a
whole binary run, not a microbenchmark runner, and it wants the `key=value`
stderr contract rather than criterion's output. There is currently no way to
execute a criterion bench in this workspace.

**Blocker C - no criterion dependency.** `Cargo.lock` contains no criterion.
Adding it churns the lockfile.

**Blocker D - the golden must not be a reimplementation.** `notes/todo.md`
already records the hazard in the `analysis/` entry: the dwell statistic is
computed twice against one definition, so the two can drift and the gate still
passes. A fill-distribution golden computed by a hand-rolled "walk the tape and
count prints" loop would repeat that mistake exactly. The golden has to be
produced by the shipped `count_penetrations` and the shipped
`Engine::apply_scans`, or it certifies nothing.

**Blocker E - no bless mechanism, and none of the obvious ones are legal
here.** An env-var bless switch is forbidden by the spec stance ("no env-var
scaffolding ... left as the way forward"). An `#[ignore]`d bless test is worse:
`brokkr.toml`'s gate profile sets `include_ignored = true`, so the gate would
run the blesser and silently overwrite the very file it is meant to check.

### What is already true and must not be broken

- `GeneratedSource` is deterministic given `(scalars, seed, origin,
  fingerprint)`, and the server folds the symbol into the seed via FNV-1a-64.
  The committed fingerprint is the only input. So a fill distribution measured
  at a fixed origin over a fixed order population is **exactly reproducible**,
  which is what makes a byte-exact golden possible rather than a tolerance band.
  `clean_regime_is_byte_identical` in `mogwai-data` is the existing precedent.
- `penetration_ticks == 0` spawns no sweeper at all - not a task, not a timer.
  Nothing in this spec may add cost to that path.
- `mogwai-engine` never sees a tick. That invariant is load-bearing and this
  spec does not touch it.

## Obstacle resolutions

**A + D, resolved by moving the walk into `mogwai-data`.** The penetration walk
is tape logic: a bounded drain of a `TickSource` plus a price predicate. Its
only server-shaped dependency is *constructing* the history source. Split it:
`mogwai-data` gets a `penetration` module owning the walk over a
`&mut dyn TickSource`, and `mogwai-server/src/fills.rs` keeps its current
signature as a thin wrapper that builds the source and delegates. The bench then
lives in `mogwai-data`, against a real `GeneratedSource`, calling the shipped
function - no lib target for `mogwai-server`, no reimplementation. This is also
the honest fix for the duplicated `through` predicate, since a shared walk needs
a shared predicate.

`through` itself is promoted to `mogwai-protocol` (next to `Side`, in
`messages.rs`), which is the only crate both `mogwai-engine` and `mogwai-data`
depend on. Both private copies are deleted. Two copies of a predicate whose own
doc comment warns they must agree is a defect, not a style point.

**B, resolved by shipping the benches as example targets.** `brokkr run`
discovers every `bin` and `example` in the workspace by target name, defaults to
release, and forwards everything after `--` raw to the program. Criterion's
`criterion_main!` parses its own argv and runs benchmarks when passed `--bench`.
So an example target containing `criterion_main!` is executed as:

```
brokkr run fill_walk_bench -- --bench
```

Examples link dev-dependencies, so criterion is a dev-dependency and never
enters the shipped dependency graph. `mogwai-data/examples/peek.rs` is the
existing precedent for an example target in this workspace. No `[[bench]]`
sections, no `harness = false`, no raw cargo.

Two sub-obstacles, both handled at the source:

- `criterion_group!` expands to `pub fn` items in a crate where nothing is
  reachable from outside, which trips the workspace's `unreachable_pub = deny`.
  Each bench example opens with `#![allow(unreachable_pub)]`; a crate-level
  `allow` overrides a lint denied through `[lints]`.
- Criterion refuses to run outside a supported harness unless its
  `cargo_bench_support` feature is on. It is on by default; the dependency is
  declared `default-features = false, features = ["cargo_bench_support"]` so
  `plotters` and `rayon` (HTML reports and parallel execution, neither wanted)
  stay out of the lockfile. If landing L3 finds criterion still refusing the
  example invocation, the fallback is to drop `default-features = false` and
  accept the extra tree; the bench shape does not change either way.

**C, accepted.** The lockfile grows criterion and its dev-only tree. It is
committed with the landing, per the standing rule.

**E, resolved by write-on-absence-and-fail.** The golden test compares against
`crates/mogwai-server/tests/golden/fill_distribution.json` when it exists, and
when it does not it **writes the file and then fails**, printing the path and a
one-line summary. Regeneration is therefore an explicit operator act - delete
the file, run the test, read the failure that tells you it was rewritten,
inspect the diff, run again - with no env var, no ignored test, and no way for
the gate to re-bless itself. Writing without failing would have been the same
hole in a different shape: a golden that is merely missing, whether dropped in a
checkout or never committed, would then be green forever and the guard silently
absent. Section 5 details the shape gate that runs before the write, so a broken
build cannot produce an artifact worth committing either.

## Target artifacts

### 1. `mogwai-protocol` - the shared predicate

In `crates/mogwai-protocol/src/messages.rs`, next to `Side`:

```rust
/// True only when a traded price is strictly through a resting limit.
///
/// The single definition of the penetration predicate. `mogwai-engine` applies
/// it to the acceptance-time market reading (seeding an aggressive limit with
/// one penetration) and `mogwai-data`'s walk applies it to every print in the
/// swept span; a print AT the limit is the market touching, not trading
/// through, and at-touch filling is the fidelity failure the gate removes. The
/// two sides of that seam must agree exactly - an order that filled on arrival
/// under one copy and was judged not-penetrated by the other would never
/// resolve - so there is one copy and both call it. Deliberately a TRADE
/// predicate, not a quote predicate: this venue has a trades-only tape.
#[must_use]
pub fn trades_through(side: Side, limit: Decimal, traded: Decimal) -> bool {
    match side {
        Side::Buy => traded < limit,
        Side::Sell => traded > limit,
    }
}
```

`messages.rs` is a private module (`crates/mogwai-protocol/src/lib.rs:18` is
`mod messages;`), so the function is invisible outside the crate until it is
added to the existing `pub use messages::{...}` block in `lib.rs`. That
re-export is part of this artifact, not an implementation detail: call sites
name it as `mogwai_protocol::trades_through`, never
`mogwai_protocol::messages::trades_through`.

Deletions: `mogwai-engine/src/orders.rs::through`,
`mogwai-server/src/fills.rs::through`. Both call sites switch to
`mogwai_protocol::trades_through`.

A serde round-trip is not applicable (this is a predicate, not a wire type), so
its gate is a unit test in `mogwai-protocol` pinning the four boundary cases -
buy strictly through, buy at touch, sell strictly through, sell at touch - named
`trades_through_is_strict_on_both_sides`.

### 2. `mogwai-data` - the walk

New file `crates/mogwai-data/src/penetration.rs`, declared `mod penetration;`
(private) with `pub use penetration::{PenetrationScan, Walk, count_penetrations};`
in `lib.rs`. That is the crate's existing convention - `mod bars; mod generated;`
are both private and every item is surfaced through a `pub use` - so the public
call sites are `mogwai_data::count_penetrations`, `mogwai_data::Walk` and
`mogwai_data::PenetrationScan`, with no `penetration::` path segment anywhere.
Sections 3 and 4 use those flat paths.

```rust
/// One resting limit the walk counts prints for.
///
/// A tape-shaped mirror of `mogwai_engine::PendingScan`, carrying only the
/// three fields the predicate reads plus the count still owed. It is not that
/// type: `mogwai-data` does not depend on `mogwai-engine` (the dependency runs
/// the other way through the server), and the engine's scan additionally
/// carries the order identity and revision that the tape has no business
/// seeing.
#[derive(Debug, Clone, Copy)]
pub struct PenetrationScan {
    pub side: Side,
    pub price: Decimal,
    /// Exclusive lower bound of the span still to walk.
    pub from_ns: u64,
    /// Penetrations still required. A zero-remaining scan is counted for and
    /// ignored, never treated as satisfied.
    pub remaining: u32,
}

/// What one walk found. Field-for-field the type `mogwai-server::fills::Walk`
/// used to be; the server now re-exports this one.
#[derive(Debug, Clone)]
pub struct Walk {
    pub counted: Vec<u32>,
    pub reached_ns: u64,
    pub drained: usize,
}

/// Count strictly-through prints for every scan in one walk of one tape.
///
/// The source is supplied already positioned by the caller, so this function is
/// tape-agnostic: the server hands it the same history source `/trades` pages
/// through, tests hand it a `MemorySource`, the bench hands it a bare
/// `GeneratedSource`. `budget` bounds the drain, because `GeneratedSource`
/// never ends and a far-from-market order would otherwise walk forever.
///
/// ONE walk per tape, not per order: every scan on a symbol shares the tape and
/// the pass span. The scans' `from_ns` may differ; each counts only prints
/// after its own bound. Synchronous and CPU-bound.
///
/// Returns as soon as every scan has reached its `remaining`, or when the span
/// `(earliest_from_ns, to_ns]` is covered, or when `budget` is spent -
/// whichever comes first. `reached_ns` is where the drain ACTUALLY got to, so
/// a truncated pass loses no span.
#[must_use]
pub fn count_penetrations(
    source: &mut dyn TickSource,
    scans: &[PenetrationScan],
    to_ns: u64,
    budget: usize,
) -> Walk;
```

The body is the current `fills.rs` loop verbatim, with `through` replaced by
`mogwai_protocol::trades_through`, the `earliest` computed as
`scans.iter().map(|s| s.from_ns).min().unwrap_or(to_ns)`, and the empty-scan
case returning `Walk { counted: Vec::new(), reached_ns: to_ns, drained: 0 }`.
The drain budget is a parameter, not a constant: it is sweep policy and belongs
to the server.

The empty-scan branch is unreachable from the server wrapper, which returns
`None` at `scans.iter().min()?` before it can be hit. It is a total-function
obligation of the extracted signature, exercised only by the `mogwai-data` test
below - noted here so nobody later reads it as live behaviour and reasons about
what the sweeper does with an empty batch.

New tests in that module, against `MemorySource` so they pin the walk itself
rather than the generator. Names are deliberately distinct from the nine server
tests, which stay where they are: `fills.rs` already owns
`counts_only_prints_strictly_through_the_limit`,
`a_truncated_drain_reports_where_it_stopped` and
`one_walk_serves_every_probe_on_a_symbol`, and two test functions with the same
name in the workspace make every `brokkr test <NAME>` filter over them
ambiguous.

- `walk_counts_only_prints_strictly_through` (both sides, at-touch excluded)
- `walk_with_a_spent_budget_reports_where_it_stopped`
- `walk_stops_at_an_exact_boundary_without_pulling_past_it`
- `walk_batches_every_scan_into_one_pass` (batched result equals per-scan
  results)
- `walk_over_an_empty_scan_list_pulls_nothing`

### 3. `mogwai-server` - the wrapper

`crates/mogwai-server/src/fills.rs` keeps `SWEEP_DRAIN_BUDGET`, keeps
`last_trade_at_or_before`, re-exports `mogwai_data::Walk`, and
`count_penetrations` becomes:

```rust
pub(crate) fn count_penetrations(
    symbol: &str,
    scans: &[PendingScan],
    to_ns: u64,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<Walk> {
    let earliest = scans.iter().map(|scan| scan.from_ns).min()?;
    let mut source = source::build_history_source(
        symbol,
        Some(earliest.saturating_add(1)),
        None,
        profiles,
        data_origin,
    )?;
    let mapped: Vec<PenetrationScan> = scans
        .iter()
        .map(|scan| PenetrationScan {
            side: scan.side,
            price: scan.price,
            from_ns: scan.from_ns,
            remaining: scan.remaining,
        })
        .collect();
    Some(mogwai_data::count_penetrations(
        source.as_mut(),
        &mapped,
        to_ns,
        SWEEP_DRAIN_BUDGET,
    ))
}
```

The `None`-on-unreachable-seek contract, the doc comments explaining
one-walk-per-symbol, the clean-tape choice, and the `/trades`-parity property
all stay here, because they are properties of *this* composition, not of the
walk. `sweeper.rs` is untouched.

All nine existing `fills.rs` tests stay exactly as they are, verbatim, and
nothing is deleted from that module. Eight of them drive `count_penetrations`
(`last_trade_at_or_before_never_looks_ahead` does not); after the move they test
the *composition* - real generator, real `build_history_source`, real budget -
which is what they were always for. The five `mogwai-data` tests in section 2 are
therefore ADDITIONS, not relocations: they pin the same edge cases one layer
down against a `MemorySource`, where a failure names the walk rather than the
whole stack. Saying they "move" would be wrong, and it is why their names are
disjoint from the server's. `the_counted_prints_are_the_prints_trades_serves` in
particular has no `mogwai-data` counterpart at all - it is the parity property
between the gate and the client's view of the tape, and only the server can
state it.

### 4. The benches

`criterion = { version = "0.8", default-features = false, features = ["cargo_bench_support"] }`
as a dev-dependency of `mogwai-engine` and `mogwai-data`, declared once in
`[workspace.dependencies]` and referenced with `.workspace = true`, matching the
file's existing convention.

**Unverified until L3 opens**: the `0.7` major, and the claim that
`default-features = false` plus `cargo_bench_support` keeps `plotters` and
`rayon` out of the tree. Neither was confirmed against the registry while
writing this - `Cargo.lock` contains no criterion, so there was nothing in-tree
to read. The first act of L3 is to confirm both against the published manifest
and correct this line; if the feature names differ, take whatever combination
yields `cargo_bench_support` on and HTML reports off, and if no such combination
exists, take the default feature set and record the extra lockfile tree as
accepted cost (obstacle B already names this fallback). The example-target trick
itself and the `#![allow(unreachable_pub)]` override do not depend on the
answer.

**`crates/mogwai-engine/examples/fill_bench.rs`** - target name `fill_bench`.

```rust
#![allow(unreachable_pub)]
```

Seven benchmarks. **Every one of them uses `iter_batched` with
`BatchSize::SmallInput`, and every one builds its engine in the setup closure**,
not once outside the whole benchmark. The engine is stateful in ways that grow
without bound across iterations: `seen_client_order_ids` retains every accepted
id for duplicate detection, `record_closed` retains terminal orders, `record_fill`
grows the fill history, and under the gate `self.open` grows by one resting order
per submit. Reusing one engine across iterations would price a monotonically
growing structure and report the average of a ramp as "submit latency". The
client order id and the `SubmitOrder` value are constructed in the setup closure
too, so neither id formatting nor `Decimal` parsing lands inside the timed
region.

| id | what it prices | input |
|---|---|---|
| `submit_full_fill` | `Engine::process_with_market(SubmitOrder, ts, None)` on the ungated default path (`penetration_ticks = 0`): validation, `plan_fill`, `commit_fill`, ledger apply, `record_fill`, snapshot | fresh unfunded engine and fresh order per iteration |
| `submit_gated_rest` | the same call with `penetration_ticks = 1` and `market_px = None`, so `seeded == 0` and the order rests - the resting branch a gated venue pays per submit | as above |
| `submit_gated_seeded` | the same with `penetration_ticks = 1` and `market_px = Some(px)` chosen so `trades_through` holds, so `seeded == 1`, the gate is satisfied on arrival and the submit fills synchronously | as above, plus a fixed marketable `px` |
| `apply_scans_50` | `Engine::apply_scans` over 50 resting orders, none crossing its threshold (the common pass) | 50 resting GTC limits, engine and scans rebuilt per iteration |
| `apply_scans_200` | the same at 200 | as above |
| `apply_scans_50_all_fill` | the 50-order batch with every result at threshold - the worst case, 50 fills plus one snapshot | as above |
| `apply_scans_200_all_fill` | the same at 200 | as above |

`submit_gated_seeded` exists because `Engine::process` passes no market price, so
a bench built only on `process` can never reach the `market_px.is_some_and(...)`
seed - the branch the real path takes on every gated submit, since `http.rs` and
the socket handler both supply a reading. Pricing only the `None` arm would
leave the shipped branch unmeasured.

The two sizes per `apply_scans` shape are the point, not redundancy. The survey
records `apply_scans` as a linear scan of `self.open` per result, so a full batch
is quadratic in the resting-order count; one size prices a point and says nothing
about the shape. 50 and 200 differ by 4x, so a quadratic term shows as roughly
16x and a linear one as 4x, which is a verdict readable off two numbers.
`reference/performance.md` records the ratio alongside the absolute figures.

Every input is fixed and constructed from `default_instruments()`; no randomness
enters a timed region.

**`crates/mogwai-data/examples/fill_walk_bench.rs`** - target name
`fill_walk_bench`. Same crate-level allow.

| id | what it prices | input |
|---|---|---|
| `walk_one_pass_1_scan` | `count_penetrations` over a one-second span with a single far-from-market scan - the per-pass floor | pre-positioned `GeneratedSource` at the BTCUSDT anchor scalars, seed 42, fixed origin, **cloned per iteration** |
| `walk_one_pass_50_scans` | the same span with 50 scans - prices the per-scan inner loop against the per-tick cost | as above |
| `walk_one_pass_500_scans` | the same span with 500 scans - the shape the inner loop's linear factor shows up in, if it does | as above |
| `scan_mapping_50` | building the `Vec<PenetrationScan>` the server wrapper builds each pass, from 50 tape-shaped inputs, allocation included | 50 fixed scan tuples |
| `source_positioning` | source construction the way the server does it - a checkpoint-backed bounded generator wrapped in `MergeSource::starting_at(.., Some(start))` behind a `Box<dyn TickSource>` - plus the seek to a fixed instant | fixed origin and target |

**The source must be cloned per iteration.** A `TickSource` is consumed as it
drains: a source positioned once outside the timed closure is walked to `to_ns`
by iteration 1, and iteration 2 pulls a tick already past `to_ns`, trips the
over-boundary break on its first pull, and returns. Criterion would then report
the cost of one `next_tick` as the cost of a pass, and the number would look
excellent. `GeneratedSource` is `#[derive(Clone)]` precisely so this is cheap
(`crates/mogwai-data/src/generated/source.rs:37` records the `ChaCha12Rng`-over-
`StdRng` choice as being for `Clone`), so every walk benchmark is `iter_batched`
with the clone in the setup closure - the same mutation hazard, and the same
remedy, as the `apply_scans` benchmarks above.

`scan_mapping_50` exists to make L2's accepted cost bound actually measurable.
L2 adds one `PendingScan` to `PenetrationScan` mapping allocation per symbol per
pass and asserts it must not show above noise; but every `walk_one_pass_*`
benchmark receives already-constructed `PenetrationScan`s, so the mapping is
outside the timed region and could never appear in those readings no matter how
expensive it got. The verdict is the ratio `scan_mapping_50 / walk_one_pass_50_scans`,
recorded in `reference/performance.md`. The benchmark cannot use
`mogwai_engine::PendingScan` as its input type - `mogwai-data` does not depend on
`mogwai-engine` and must not start - so it maps from a field-identical local
tuple. That is a faithful proxy for the allocation and the per-field copy, which
is the whole cost; it is not a proxy for anything else, and performance.md says
so.

`source_positioning` is separated deliberately: the sweeper's cost is
`positioning + walk`, and the two scale with completely different things (the
first with the seek distance, the second with print density and scan count).
Reporting one number for both would hide which one regressed. It must build the
source the way `fills::count_penetrations` does, not as a bare `GeneratedSource`:
the real per-pass fixed cost is a checkpoint lookup, a `BoundedSeek` drain, a
`MergeSource` wrapper and a `Box<dyn>` indirection, and a bare generator prices
none of them. Two ingredients of `build_history_source` are not reachable from
`mogwai-data` - the `InstrumentProfiles` lookup and the server's `seed_for`
symbol folding - and both are constant-time table reads outside any loop;
`reference/performance.md` names them as the known gap between this reading and
the server's true fixed cost rather than leaving the difference implicit.

### 5. The golden fill distribution

New file `crates/mogwai-server/src/fill_golden.rs`, declared from `main.rs` as
`#[cfg(test)] mod fill_golden;`. It lives in the bin crate because it is the
only place that can see `fills::count_penetrations`, `Engine::apply_scans` and
`source::InstrumentProfiles` at once - which is precisely the seam being
certified.

The harness reproduces `sweeper.rs`'s three-phase pass synchronously:

```rust
struct Scenario {
    penetration_ticks: u32,
    sweep_interval_ns: u64,
    /// Limit placement, in price increments away from the market at
    /// acceptance. Always >= 1 so no order is marketable on arrival and every
    /// fill's timing is a function of the tape rather than of the seed.
    offset_ticks: Vec<u32>,
    /// Orders per offset.
    orders_per_offset: usize,
    accept_stride_ns: u64,
    horizon_ns: u64,
}
```

Fixed values, committed as constants and echoed into the golden's `config`
block so a stale golden is self-describing:

- `symbol = "BTCUSDT"`, `data_origin_ns = 1_700_438_400_000_000_000` (the
  `TEST_ORIGIN` the existing `fills.rs` tests already use)
- `penetration_ticks` in `{1, 3}` - one scenario each
- `sweep_interval_ns = 1_000_000_000` (1 s simulated)
- `offset_ticks = [1, 10, 100, 1_000, 10_000]`
- `orders_per_offset = 40` (200 orders per scenario),
  `accept_stride_ns = 3_000_000_000` (3 s)
- `horizon_ns = 1_200_000_000_000` (20 simulated minutes)
- every order is `OrderType::Limit`, `TimeInForce::Gtc`, `quantity = 1`

**Time in force is not incidental.** `Engine::pending_scans`
(`crates/mogwai-engine/src/lib.rs:342`) filters to `OrderType::Limit` AND
`TimeInForce::Gtc`; anything else produces no scan, so an IOC or FOK population
would yield an empty `pending_scans` every pass and censor every cell while
looking like a legitimate distribution. GTC limits are the only population this
harness can be built from, and the `Scenario` carries no knob to vary it.

**Why the offsets are decade-spaced.** BTCUSDT's `price_increment` is
`Decimal::new(1, 2)` = 0.01 (`crates/mogwai-protocol/src/instruments.rs:35`)
against a fitted tape near 37,000. The original `[1, 2, 4, 8, 16]` spanned one
cent to sixteen cents - under half a basis point at the widest, so every cell
would fill on the first or second pass, `censored` would be zero everywhere, and
L4's eyeball criteria (monotone in `offset_ticks`, `penetration_ticks = 3`
slower than `= 1`) would be comparisons between noise and noise. The decade
ladder spans one cent to 100 dollars, roughly 0.003 to 27 basis points, so the
tape decides the outcome and the far cells genuinely censor. The ladder also
lets the horizon and the population shrink, which is what makes the runtime
bound below reachable.

**What the second scenario actually covers.** `penetration_ticks = 3` covers
multi-pass penetration ACCUMULATION: an order that must be traded through on
three separate prints, crossing sweep-pass boundaries, with the frontier
advancing each pass. It does NOT cover a restarted window. The engine restarts
the window only when a fill leaves a remainder
(`crates/mogwai-engine/src/orders.rs:248-259`), which requires a non-unit
`fill_fraction`, which requires an armed `PartialFillNext`. This harness arms no
divergence at all, so every admitted order fills completely and leaves the book.
Any claim of restart coverage would be false; the restart path is explicitly out
of scope for this golden, and adding a partial-fill cohort would mean defining
how multiple tranche latencies per order appear in the artifact, which is a
different distribution and a different spec.

**The exact schedule.** Two conforming implementations must produce the same
golden, so nothing here is left to the implementer. Orders are indexed
`i` in `0..(offset_ticks.len() * orders_per_offset)`, i.e. `0..200`, and for
each `i`:

- `offset_index = i % offset_ticks.len()`, so offsets round-robin and every
  offset's population is spread evenly across the whole acceptance window rather
  than clustered in one contiguous block of tape;
- `accept_ns = data_origin_ns + (i as u64) * accept_stride_ns`. The last
  acceptance is therefore at `199 * 3 s = 597 s`, leaving every order at least
  half the 1,200 s horizon in which to fill. **The harness asserts
  `(total_orders - 1) * accept_stride_ns * 2 <= horizon_ns` before it starts**,
  so a later edit to any of the three constants cannot silently push acceptances
  past the horizon and censor a whole tail as if the tape had done it;
- `side = if i % 2 == 0 { Side::Buy } else { Side::Sell }`, so both predicate
  directions are covered and the two sides are balanced within every offset cell
  (offsets round-robin over five values and sides over two, and 5 and 2 are
  coprime, so each offset sees both sides equally);
- `client_order_id = format!("g{penetration_ticks}-{i}")` - unique across the
  whole run so no submit is refused as a duplicate, and stable across
  regenerations;
- the order is submitted at the first pass whose `ts >= accept_ns`, and because
  `accept_stride_ns` is an exact multiple of `sweep_interval_ns` that is the
  pass at exactly `accept_ns`. Latency is measured against `accept_ns`, not
  against the pass instant, and the two are equal by construction.

Ambiguity here is not a style matter: the earlier wording permitted both "one
global order every 4.5 s" (acceptance spanning 75 minutes, past a 30-minute
horizon, silently censoring the tail) and "one order per offset every 4.5 s"
(five orders sharing every acceptance instant). Those produce different goldens
from the same document.

Per scenario the harness:

1. builds one `Engine` from
   `EngineConfig { account_id, instruments: default_instruments(), balances: HashMap::new(), penetration_ticks }`
   - an empty balance map, so `enforce_funds` is off and no order can be
   rejected for funds, which would silently thin a cell;
2. steps `ts` from `data_origin_ns` in `sweep_interval_ns` increments to
   `data_origin_ns + horizon_ns`;
3. at each step, first submits any order whose acceptance instant has arrived,
   through `engine.process_with_market(ClientMessage::SubmitOrder(..), ts,
   fills::last_trade_at_or_before(symbol, ts, &profiles, origin))` - the same
   market reading `http.rs` gives the real path, so the acceptance-time seed is
   the shipped one. Its limit price is that reading offset by
   `offset_ticks * price_increment` away from the market (down for a buy, up for
   a sell), which is on the grid because the tape price is. Sides alternate per
   order so both predicate directions are covered;
4. then runs the pass: `engine.pending_scans()`, one
   `fills::count_penetrations` call for the single symbol, `ScanResult`s built
   exactly as `sweeper.rs` builds them, `engine.apply_scans(&results, ts)`;
5. records, from each `ServerMessage::OrderFilled` emitted, the fill's
   `ts_event` minus that order's acceptance instant, and the number of passes
   elapsed.

Orders unfilled at the horizon are **censored**, counted, and excluded from the
sample rather than folded in at the horizon value - a censored order says
"longer than 20 minutes", and folding that in would make the golden move
whenever the horizon moved.

The artifact, `crates/mogwai-server/tests/golden/fill_distribution.json`. That
path is deliberate and stays: `crates/mogwai-server/tests/` already exists and
already holds `daemon.rs`, so this is not a `tests/` tree conjured up for a
fixture, and cargo auto-discovers integration targets only from top-level `.rs`
files in `tests/`, so a `golden/` subdirectory is inert. The fixture sits with
the crate's other test assets rather than in a second parallel directory.

```json
{
  "schema": 1,
  "symbol": "BTCUSDT",
  "data_origin_ns": 1700438400000000000,
  "sweep_interval_ns": 1000000000,
  "horizon_ns": 1200000000000,
  "orders_per_offset": 40,
  "accept_stride_ns": 3000000000,
  "cells": [
    {
      "penetration_ticks": 1,
      "offset_ticks": 1,
      "samples": 40,
      "filled": 37,
      "censored": 3,
      "buy_filled": 19,
      "sell_filled": 18,
      "latency_ns": [0, 0, 0],
      "passes": [0, 0, 0]
    }
  ]
}
```

**The cell carries the whole sample, not a summary.** `latency_ns` is every
filled order's latency in that cell, sorted ascending; `passes` is the matching
sorted vector of elapsed pass counts. The deliverable is named a fill
DISTRIBUTION, and four order statistics are not one: p10/p50/p90/max are
unchanged by any rearrangement of the mass between them, so a generator change
that moved a third of the orders across a pass boundary could leave the file
byte-identical while the distribution moved - which directly falsifies the
"any change that moves fill timing moves this file" claim the exact-comparison
design rests on. Committing the full sample is cheap here: 400 orders total
across both scenarios, two integer vectors per cell, ten cells - a file in the
low tens of kilobytes, diffable, and with no quantile arithmetic to get wrong or
to re-derive when reading a failure. Quantiles, if they are ever wanted for
`reference/performance.md`, are computed from the committed vectors rather than
stored beside them, so there is one definition and not two.

Sorting the sample rather than emitting it in order of client id is deliberate:
submission order is an artifact of the schedule, and a golden keyed on it would
churn on any schedule edit that did not change the distribution at all. Sorting
also makes the vectors read as a discrete CDF, which is how a reviewer eyeballs
a failure.

`buy_filled` and `sell_filled` sum to `filled` and exist so a one-sided
regression - an inverted predicate, a seeding rule that only fires for buys -
cannot hide behind an unchanged total.

Every value in the file is an integer, so the comparison is exact string
equality on canonical JSON. No float formatting, no tolerance, no
approximate-equality helper. The generator is deterministic given the committed
fingerprint, the fixed origin and the fixed order population, so exact is the
correct strength: any change to the fingerprint, the generator, the predicate,
the seeding rule or the frontier arithmetic moves this file, and any change that
does not move it did not change fill timing.

Serialization is `serde_json::to_string_pretty` over an ordered struct (not a
map), so field order is fixed by the type. The file ends with a newline.

**One test, `fill_distribution_matches_the_golden`**, because the sweep is the
expensive part and there is exactly one of it. The earlier two-test split had
`every_cell_has_a_filled_sample` as a second `#[test]` with no way to share the
run, which is two full sweeps for one measurement - and on the golden's own
runtime budget that is the difference between fitting the gate and not. The
shape assertions run inside the single test, in this order:

1. Run both scenarios and render the JSON.
2. **Shape gate, asserted against the freshly computed result before any
   comparison or write.** Every one of these is a property of correct code, not
   of a particular tape:
   - exactly `2 * offset_ticks.len()` cells, in a fixed
     `(penetration_ticks, offset_ticks)` order;
   - every cell's `samples == orders_per_offset`, and
     `filled + censored == samples`;
   - `latency_ns.len() == filled` and `passes.len() == filled` in every cell;
   - both sides participate: `buy_filled > 0` and `sell_filled > 0` in the
     nearest cell of each scenario (`offset_ticks == 1`);
   - the nearest cell of each scenario fills a majority of its sample
     (`filled * 2 > samples`), which is much stronger than "at least one fill"
     and is what actually catches an inverted predicate or a frontier that never
     advances - a broken build that manages to fill a single order, or only
     buys, would have passed the old guard.

   A failure here fails the test outright and never writes or compares, so
   broken code cannot produce a blessable artifact.
3. Compare against the committed file if it exists. On mismatch, fail naming the
   first differing cell and, within it, the first differing index, plus the
   instruction: delete the file, re-run this test, read the diff.
4. **If the file is absent: write it, print `wrote <path> (<n> cells)`, and
   FAIL** with a message saying the artifact was regenerated and must be
   inspected and committed, then re-run. Absence is not a pass. A missing golden
   is indistinguishable from a golden someone forgot to commit or a checkout
   that dropped it, and under the old bootstrap-passes rule that state is green
   in CI forever - the guard would be silently absent rather than loudly broken.
   Failing on write preserves the whole delete-and-rerun ergonomic (the operator
   deletes, runs, gets a failure that tells them the file is written, inspects
   the diff, runs again, green) while making the absent state impossible to
   ignore. It also removes the last self-bless hazard, which in turn makes the
   `#[ignore]` option in L4's runtime discussion safe to take.

Reading the committed artifact is also what the shape gate would have been
guarding if it were a separate test, so nothing is lost by folding it in:
step 2's assertions hold for the rendered result, and step 3 proves the
committed file equals that result, so the committed file inherits every one of
them.

### 6. `reference/performance.md`

Created by this spec, because the technical-implementation-spec names it as the
durable record of measured numbers and it does not exist. Initial content: what
each bench id measures, the exact command to run it, the machine and commit the
first reading was taken on, and the readings themselves as a table. Every later
fill-path change that moves a number appends a row rather than editing one, so
the file is a history and not a snapshot.

Three things it must record beyond the raw numbers, because they are the parts a
later reader would otherwise have to re-derive:

- the `apply_scans_200 / apply_scans_50` ratio, which is the scaling verdict on
  the quadratic shape (roughly 4x linear, roughly 16x quadratic);
- the `scan_mapping_50 / walk_one_pass_50_scans` ratio, which is L2's accepted
  cost bound made concrete;
- the two named gaps in `source_positioning` - it omits the server's
  `InstrumentProfiles` lookup and `seed_for` symbol folding, both constant-time
  table reads outside any loop - so nobody later reads that number as the
  server's complete per-pass fixed cost.

If a benchmark is deleted for missing the 5 percent bar (L3), its row is not
removed from `reference/performance.md`. The row stays with the reading it
achieved and a one-line note that the bench was deleted as unusable at that
granularity, because the fact that a quantity was measured and found
unmeasurable-to-3-percent is itself the durable finding - deleting the row would
guarantee the next person tries the same bench.

The file also states the honest limit of this guard: criterion's saved baselines
live under `target/`, which is gitignored and not portable between machines, and
a wall-clock assertion inside `brokkr check` would be flaky on any shared
machine. So **throughput has no automatic gate**. The benches are operator-run
before and after a change that touches the fill path, and `reference/performance.md`
is where the two readings are compared. Behaviour, by contrast, *is*
automatically gated - by the golden file, which runs in `brokkr check`.

## Landings

Four landings. Each is one coherent intrusive change, each leaves the suite
green, each is independently revertible.

### L1 - one predicate

Add `mogwai_protocol::trades_through` plus its unit test; delete both private
`through` copies; repoint the two call sites.

Gate:

```
brokkr check
brokkr test -p mogwai-protocol trades_through_is_strict_on_both_sides
brokkr test -p mogwai-server counts_only_prints_strictly_through_the_limit
```

Expected output change: none. This landing is behaviour-preserving by
construction - the two deleted bodies are identical to the added one - and the
existing at-touch tests in `fills.rs` are what prove it.

Keep/revert: revert if any existing test changes verdict. There is no
performance dimension.

### L2 - the walk moves to `mogwai-data`

Add `penetration.rs` and its five tests; reduce `fills::count_penetrations` to
the wrapper; leave `sweeper.rs`, the nine server tests, and the whole
`SWEEP_DRAIN_BUDGET` policy where they are.

Gate:

```
brokkr check
brokkr test -p mogwai-data walk_
brokkr test -p mogwai-server sweep_pass_walks_only_the_new_span
brokkr test -p mogwai-server a_pass_costs_one_walk_per_symbol_not_per_order
brokkr test -p mogwai-server the_counted_prints_are_the_prints_trades_serves
```

`brokkr test` takes a case-sensitive SUBSTRING over the full test path, so the
filter has to appear in the names as written: all five new tests begin `walk_`
and nothing else in `mogwai-data` does. A filter of `count_penetrations` would
match zero tests - the function's name is not in any of the test names, whose
paths are `penetration::tests::walk_*` - and a filter that matches nothing
reports success, so the command would have been a green no-op in the gate.

Plus the live path, because this landing sits under the fill seam. Both
penetration modes, in sequence, each with its own server:

```
brokkr run mogwai -- serve --config scripts/smoke-penetration.toml
python3 scripts/smoke.py --penetration
brokkr run mogwai -- stop
brokkr run mogwai -- serve --config scripts/smoke-penetration-two.toml
python3 scripts/smoke.py --penetration-swept
brokkr run mogwai -- stop
```

The mode flags are load-bearing: `scripts/smoke.py` with no argument dispatches
to `main_default` (`scripts/smoke.py:1144`), which exercises the ungated path and
would pass unchanged no matter what this landing did to the walk. `--penetration`
and `--penetration-swept` are the two modes that touch the seam
(`scripts/smoke.py:1139-1142`), and each is paired with the config it was written
against - `smoke-penetration.toml` for the first, `smoke-penetration-two.toml`
for the two-symbol swept case.

`serve` daemonizes by default and `stop` ends it through the PID-file lock, so
the sequence above needs no job control and no second terminal; the explicit
`stop` between the two runs is what keeps the second `serve` from colliding with
the first server's PID file and ports. Use `serve -f` instead only when watching
the log live, in which case each server needs its own terminal and the `stop`
lines become interrupts.

Expected output change: none anywhere. The walk's arithmetic is unchanged; only
its address is.

The cost gates are the load-bearing part of this gate. `count_penetrations` is
on the sweeper's per-interval path, and the refactor adds one `Vec` allocation
per pass (the `PendingScan` to `PenetrationScan` mapping). That is one
allocation per symbol per interval against a walk that already pays a checkpoint
restore and a mutex acquisition, so the accepted cost bound is: **`scan_mapping_50`
must come in at least an order of magnitude under `walk_one_pass_50_scans`.**

That bound is stated against a named benchmark rather than "must not appear
above noise in the walk readings", because the mapping happens OUTSIDE every
`walk_one_pass_*` timed region - those benches take already-constructed
`PenetrationScan`s - so no `walk_one_pass_*` reading could ever show it, however
expensive it became. `scan_mapping_50` (section 4) exists for exactly this
assertion.

**L2 therefore lands with its cost bound unmeasured, and that is deliberate.**
The instrument cannot precede the refactor: `scan_mapping_50` and the
`walk_one_pass_*` family both call the extracted `mogwai_data::count_penetrations`,
which does not exist until L2 lands, so no reordering makes the measurement
available earlier. What L2 gates on instead is the two existing server cost
tests, `sweep_pass_walks_only_the_new_span` and
`a_pass_costs_one_walk_per_symbol_not_per_order`, which pin the ALGORITHMIC cost
- one walk per symbol per pass, over the new span only - and which are the
properties a refactor could plausibly break. The constant-factor bound is
recorded as an open obligation of L3 and discharged there. If it fails at L3,
the answer is to have `pending_scans` produce the tape-shaped scan directly, not
to reunite the two crates; that is a change to `mogwai-engine`'s scan type, not
a revert of L2.

Keep/revert: revert as a unit if the smoke run diverges or a cost gate trips.

### L3 - the benches

Add criterion to `[workspace.dependencies]` and as a dev-dependency of the two
crates; add both example targets; create `reference/performance.md` and fill it
with the first readings.

Gate - the benches are the instrument, so running them *is* the gate:

```
brokkr check
brokkr run fill_bench -- --bench
brokkr run fill_walk_bench -- --bench
```

`brokkr check` matters here beyond habit: cargo builds example targets during
`cargo test`, so a bench that does not compile, or that trips
`unreachable_pub`/`unused`, fails the ordinary check rather than lurking until
someone runs it.

Plus the L2 obligation this landing discharges:

```
brokkr run fill_walk_bench -- --bench scan_mapping_50
brokkr run fill_walk_bench -- --bench walk_one_pass_50_scans
```

with the ratio written into `reference/performance.md` against L2's stated bound
(at least an order of magnitude under).

**Usability threshold, judged from the first reading.** Any benchmark whose
standard deviation exceeds **5 percent of its mean** is not a usable regression
gate - a later 3 percent regression would be invisible in it. Such a benchmark
is re-shaped (larger span, more iterations per batch, or a smaller timed region)
and re-measured before its number is recorded. A benchmark that cannot be
brought under 5 percent is deleted rather than committed: a bench nobody can
read a verdict off is worse than no bench, because it looks like coverage.

The verdict is read from **`target/criterion/<bench_id>/new/estimates.json`**,
field `std_dev.point_estimate`, divided by `mean.point_estimate` from the same
file. Not from the console line: criterion's default console output leads with a
confidence interval on the mean, which is a different quantity and a much
narrower one, and eyeballing the CI width in place of the deviation would pass
benchmarks this rule is meant to reject. Both figures live in that JSON in
nanoseconds, so the ratio is a two-number division with no unit parsing.

This spec makes no throughput claim and proposes no rewrite, so there is no
proceed/close threshold on a speedup. The instrument is the deliverable; the 5
percent figure above is the threshold on the instrument itself.

Keep/revert: revert if `brokkr check` cannot build the examples after the
fallback in obstacle B has been tried, or if every benchmark misses the 5
percent bar (which would mean the fill path is not measurable at this
granularity and the item closes as mispriced).

### L4 - the golden

Add `fill_golden.rs`, its single test, and the committed
`tests/golden/fill_distribution.json`. Generate the file by deleting-and-running
exactly as an operator later would (the run FAILS by design and writes the
file), then read it before committing: every cell must have a plausible non-zero
filled count, the median latency must be monotone non-decreasing in
`offset_ticks` within a `penetration_ticks` group, the far cells must show real
censoring where the near cells show none, and the `penetration_ticks = 3` cells
must be slower than their `= 1` counterparts. A golden that fails that reading
by eye is evidence of a bug in the gate, not a number to commit.

Gate:

```
brokkr check
brokkr test -p mogwai-server fill_distribution_matches_the_golden
```

**Runtime bound, measured in the profile the gate actually uses.** The harness is
1,200 passes per scenario, each a checkpoint restore plus a positioned seek plus
a walk over roughly a second of fitted BTCUSDT prints, plus one
`last_trade_at_or_before` per submitted order - each of which builds its own
history source - times two scenarios.

The earlier bound of "60 seconds in release" measured the wrong thing.
`brokkr test` always passes `--release`, but the gate this test has to fit is
`brokkr check`, whose test lane is an ordinary `cargo test` in the DEV profile.
Unoptimized ChaCha12 plus the Weibull/Normal/ChiSquared sampling in the
generator runs many times slower than the release build, so a comfortable
release figure says nothing about what every `brokkr check` will pay. The bound
is therefore: **the pair must complete in under 20 seconds under
`brokkr check`**, i.e. dev profile, and that is the number to measure first -
before the release figure, and before the golden is committed.

Three levers, applied in this order until it fits, each one requiring the golden
to be regenerated:

1. halve `horizon_ns` to 10 simulated minutes (the schedule assertion in section
   5 then also forces `accept_stride_ns` down, so the population is unchanged);
2. drop to a single scenario's worth of passes by widening `sweep_interval_ns` to
   2 s, which halves the pass count without touching the order population or the
   span of tape covered;
3. `#[ignore]` the test.

Lever 3 is a real option and not a defeat, which is a change from the earlier
draft. Obstacle E rejects `#[ignore]` for a BLESSER - correct, since
`brokkr.toml`'s gate profile sets `include_ignored = true` and the gate would run
the blesser and re-bless the file it is checking. That argument does not transfer
to the CHECKER: ignoring the checker costs nothing at the gate, because
`brokkr check --gate` runs ignored tests, and it is exactly how the four
socket-backed adapter binaries already live in this workspace. It costs coverage
only on plain `brokkr check`. And with section 5's write-on-absence-and-FAIL
rule, the test has no self-bless path left even when the gate does run it, so the
original objection is fully answered. If lever 3 is taken, the project rule that
`--gate` is required for `mogwai-adapter`-touching commits must be extended in
`CLAUDE.md` to cover fill-path commits, or the guard is one nobody runs.

Keep/revert: revert if the golden proves unstable across two consecutive
regenerations on the same commit. That would falsify the determinism premise the
whole exact-comparison design rests on, and the fallback (quantile bands with a
tolerance) is a different spec, not a patch to this one.

### Documentation, landed with L2 and L4

- `reference/architecture.md`, "Fills are synthetic": one sentence recording
  that the strictly-through predicate is now a single definition in
  `mogwai-protocol` shared by the engine's seed path and the tape walk. Landed
  with L2.
- `reference/architecture.md`: a short note that the gated fill distribution is
  pinned byte-exactly by a committed golden, naming the file and the
  delete-and-rerun regeneration. Landed with L4.
- `reference/performance.md`: created in L3 as described.
- `notes/todo.md`: the phase D entry is deleted in L4, once both halves have
  landed. Not before - a half-landed item is still an open item.

## Stopping rule and explicit non-scope

The teardown stops at the walk. In scope: the `through` predicate, the walk's
body and its new home, the two bench example targets, the golden harness and its
file, and the four documentation edits above.

Named and excluded, not deferred:

- **`mogwai-server` does not gain a library target.** Obstacle A is solved by
  moving the walk down, not by exposing the binary's internals. Splitting a
  twelve-module `pub(crate)` bin into a lib is a real refactor with an
  `unreachable_pub` blast radius across the whole crate, and nothing in this
  item needs it.
- **`sweeper.rs` is not touched.** The golden harness reproduces its pass
  ordering rather than sharing code with it, because sharing would require the
  lib target above and because the async loop, the teardown race and the
  per-session delivery are not what a fill distribution measures.
- **The `analysis/` test-harness decision** (its own `notes/todo.md` entry) is
  untouched. This spec deliberately does not route any golden through Python:
  the fill golden is produced and checked by the same Rust code path, which is
  exactly the double-definition trap that entry is about, avoided rather than
  repeated.
- **Queue-ahead execution semantics** remain refused, per
  `reference/architecture.md`. The gated penetration config is the only shape
  these distributions will ever cover, which is what makes committing them safe.
- **No automatic throughput gate.** Justified in the
  `reference/performance.md` section above. If one is wanted later it needs a
  stable measurement host, which is a project-shape decision and a separate
  item.
- **No new divergence, no new config knob, no wire change.** `penetration_ticks`
  and `fill_sweep_interval_ms` are sufficient inputs for everything here.
- **The window-restart path is not covered.** It needs a `PartialFillNext`
  cohort and a definition of how per-tranche latencies enter the artifact; see
  the second-scenario note in section 5.

## Review disposition

Two independent reviews were run against the pre-revision draft and consolidated
into the text above. Everything they raised is either folded in or listed here
with the reason it was not.

Folded in, with where: the shared-predicate re-export from
`mogwai-protocol/src/lib.rs` (section 1); the flat `pub use` module convention
and the disjoint `mogwai-data` test names (section 2), and with them the
correction that the five new tests are additions rather than relocations
(section 3); the unreachable empty-scan branch called out as a total-function
obligation (section 2); the criterion version and feature set marked unverified
with a stated L3 confirmation step (section 4); `iter_batched` and per-iteration
state for BOTH bench families, including the consumed-`TickSource` hazard the
draft caught for `apply_scans` and missed for the walk (section 4); the
`submit_gated_seeded` benchmark that reaches the `process_with_market` seed
branch (section 4); a second `apply_scans` size so the quadratic shape is
measurable (section 4); `scan_mapping_50` and a realistic `source_positioning`,
plus the admission that L2's cost bound cannot be measured before L2 lands
(sections 4 and L2); decade-spaced offsets against the real 0.01
`price_increment` (section 5); explicit GTC, exact per-order schedule formulas
and a horizon-versus-acceptance assertion (section 5); the corrected claim about
what `penetration_ticks = 3` covers (section 5); the full sorted sample plus
per-side fill counts in place of six order statistics (section 5); one test
instead of two, with a strengthened shape gate that runs before any write
(section 5); write-on-absence-and-FAIL (obstacle E and section 5); the correct
smoke modes and configs with explicit server lifecycle (L2); the
`mogwai-data` test filter that previously matched zero tests (L2); the
`estimates.json` extraction procedure for the 5 percent rule (L3); a dev-profile
runtime bound with named levers, `#[ignore]` among them (L4); and the
performance.md obligations for the two ratios, the `source_positioning` gap and
deleted benches (section 6).

Rejected:

- **"`crates/mogwai-server` has no `tests/` directory today, so the golden
  fixture should live in `goldens/`."** The premise is false -
  `crates/mogwai-server/tests/daemon.rs` exists. The stated hazard (cargo
  treating the fixture as a test target) does not apply either, since
  auto-discovery only picks up top-level `.rs` files. Keeping the fixture beside
  the crate's existing test assets is better than a second parallel directory.
  Noted in place in section 5.
- **"`brokkr check`'s test lane has a 20-second per-test watchdog."** No such
  timeout appears in `brokkr.toml` or in `brokkr man check`, and nothing else in
  the workspace references one. The dev-profile half of the same finding is
  correct and is folded in; the watchdog figure is not asserted anywhere in this
  spec. L4's 20-second bound is a budget this spec chooses, not a limit the tool
  imposes.

The two reviews overlapped on three findings - the stateful-benchmark hazard,
the false window-restart claim, and the release-versus-dev runtime bound - each
of which is folded in once, in the more precise of the two framings.
