<!--
SPDX-FileCopyrightText: 2026 folknor
SPDX-License-Identifier: AGPL-3.0-only
-->

# Piece 7: kill the process-global singletons

Implementation spec. Written against
`reference/technical-implementation-spec.md` and spawned from
`notes/todo.md` - piece 7 of "Landing the grand design: fourteen pieces"
("Kill the process-global singletons: `RunIndex`, `BOOT`, the `.next()`
collapse in `serve.rs`; per-river keyed state, lazy engine registration"),
together with the design bullets it points to under Open issues: "THE
SYMBOL IS A REQUEST PARAMETER, NOT AN IDENTITY THE VENUE OWNS" (items 5
and 7 and 8), "THE RIVER AND THE BOAT", "SYMBOL RESOLUTION IS TOTAL",
"THREE STALE COMMENTS", and the standing prose obligation.

## 1. What this piece delivers

One sentence: **the run's tape state stops being three process globals
and becomes a keyed, lazily populated registry of rivers owned by the
serving process, and the venue serves history for every configured
shape instead of for one.**

Concretely, after this lands:

- `source::INDEX` (a `static OnceLock<RunIndex>` holding ONE symbol and
  ONE `CheckpointIndex`) and `source::BOOT` (a `static
  OnceLock<BootTape>`) do not exist. Nothing in `mogwai-server` is a
  mutable process global.
- `build_instrument_profiles` returns EVERY configured shape rather than
  validating all of them and keeping only the boot one, and `serve.rs`
  stops collapsing that set with `.next()`.
- Every checkpoint chain is created ON FIRST USE, keyed, exactly once
  under contention, and locked independently of every other chain.
- The engine registers an instrument when a symbol is first bound rather
  than taking a fixed vector at construction.
- `/trades` and `/quotes` answer for any configured symbol; `/instruments`
  reports every configured shape.

What this piece deliberately does NOT deliver is in section 8.

## 2. Survey of the ground

### 2.1 The three singletons

`crates/mogwai-server/src/source.rs`:

- `struct RunIndex { symbol: Symbol, checkpoints: Mutex<CheckpointIndex> }`
  behind `static INDEX: OnceLock<RunIndex>`. The private `index(symbol,
  profiles)` helper resolves the profile, then returns the chain ONLY if
  `existing.symbol == symbol`, and `None` otherwise. Its doc comment
  ("Process global because the run is: one instrument, one regime, one
  origin ... nothing left to key it by") is the sentence this piece
  deletes.
- `static BOOT: OnceLock<BootTape>`, carrying `RunSeeds` and the optional
  `MarketRegime`. Read by `generator()` with an `expect`, written once by
  `materialize_warmup`, and installed in tests by `set_boot_for_test`,
  which exists only because the global would otherwise let whichever test
  ran first silently fix the tape for the whole binary.
- Six `pub(crate)` free functions take `(symbol, profiles)` and funnel
  through `index()`: `next_live_tick`, `activate_live`, `arm_flow_surge`,
  `clear_flow_surge`, `build_history_source` (plus its `#[cfg(test)]`
  wrapper `build_live_source`), `last_trade_at_or_before`. Every one of
  them returns `None`/`false` for a symbol that is not the initialized
  one, with no error and no log - the silent-failure surface the TODO
  names.

The whole-file consequence: `MAX_EXTEND_TICKS` and
`MAX_WARMUP_MATERIALIZATION_TICKS` are today per-process because there is
one chain; they become per-river, which is what they always meant (both
are reach bounds on ONE chain's lead, not a process budget).

### 2.2 Callers of those six functions

- `tape.rs` - `Tape::start` asserts `activate_live`, probes with
  `build_history_source(&symbol, None, ..)`, then loops `next_live_tick`.
  `Tape` holds `symbol: String` and `profiles: Arc<InstrumentProfiles>`
  purely to re-derive the chain on every call, and its `arm_flow_surge` /
  `clear_flow_surge` do the same.
- `fills.rs` - `scan_triggers_with_budget`, `read_market` and `read_last`
  each take `(symbol, .., profiles)` and call
  `build_history_source` / `last_trade_at_or_before`.
- `sweeper.rs` - `FillSweep { run, profiles, interval_ms }`; the pass
  groups pending scans by symbol and calls into `fills` per symbol. It is
  ALREADY symbol-keyed and needs no restructuring, only the new handle.
- `http.rs` - `bounded_trades` / `bounded_quotes` build a history source;
  `AppState` carries `profiles`; `mark_reading` uses the
  `MarketReadingCache`.
- `serve.rs` - `materialize_warmup` for the boot symbol.
- `fill_golden.rs` - `build_history_source(SYMBOL, Some(ORIGIN), profiles)`.

### 2.2b Callers of `build_instrument_profiles` OUTSIDE the server

Two exist, and step 3 breaks both, so they are in scope for this piece
even though section 7's stopping rule would otherwise exclude
`mogwai-lab`:

- `crates/mogwai-cli/src/gen.rs` - `let [def] = defs.as_slice() else
  { bail!("--config resolved {} instruments, expected exactly one") }`.
- `crates/mogwai-lab/src/fit/walk.rs` - the same destructure with "the
  scratch config resolved {} instruments, expected exactly one".

Today neither arm can fire, because the function collapses to one entry.
After step 3, ANY config carrying a `[symbols.*]` table alongside its
boot shape starts refusing by that message - a scratch config that
works today stops working, with an error that blames the config for the
piece's own change. Both must move to the same boot-symbol lookup
`serve.rs` uses (`boot_symbol_def`, section 3.8), resolving the shape
they mean by NAME rather than by count. This is a step-3 obligation,
not a follow-up: the destructure and `build_instrument_profiles`'s
plurality change in the same commit or the commit is red-by-behaviour.
Section 6.2 carries the test.

### 2.3 The `.next()` collapse and what it feeds

`config::build_instrument_profiles` sweeps the boot shape first and then
every `[symbols.*]` key, validating each and calling
`refuse_unfunded_settlement` on each - and then keeps only the FIRST
(`boot.get_or_insert(profile)`), returning a one-entry
`InstrumentProfiles`. So `serve.rs`'s
`profiles.instrument_defs().into_iter().next()` is not "alphabetically
first of many"; it is the only entry, and the collapse is upstream in
`build_instrument_profiles`. The TODO's framing ("effectively the
alphabetically first profile") is close but the fix is in config, not
only in `serve.rs`. That single `InstrumentDef` then reaches:

- `Run::new(instrument, ..)`, which stores `run.instrument`, looks up
  margin and fees by that symbol, starts `Tape::start(symbol, ..)` and
  builds the engine with `instruments: vec![instrument]`.
- `ReadyRecord.symbol` (written in `serve.rs`).
- `http::instruments`, which answers `Json(vec![state.run.instrument.clone()])`.
- `ws.rs`, which passes `&state.run.instrument.symbol` as `bound` to
  `resolve_socket_symbol`.

`run.instrument` has exactly two readers outside `Run::new` (the `bound`
argument in `ws.rs` and the `instruments` handler in `http.rs`), which is
why removing it is cheap.

### 2.4 Reachability semantics of a cold chain

`CheckpointIndex::try_source_before_target` calls `extend_toward(target)`
when the index is NOT live, and returns `None` when the lead still sits
below the target. `activate_live` transfers frontier advancement to the
paced worker, after which readers never extend. `extend_toward` is
bounded by `MAX_EXTEND_TICKS` per call, which is why
`materialize_warmup` loops it against
`MAX_WARMUP_MATERIALIZATION_TICKS`.

This is the fact that makes plural history cheap: a river with NO boat on
it needs no worker thread. It extends on demand from whatever reader
touches it, and a reader loop identical to warmup's is all a cold river
needs to answer any instant at or below the run clock's now.

### 2.5 Tests pinning the behaviour being deleted

- `fills.rs::initialized_run_index_refuses_every_other_symbol` builds a
  FULLY RESOLVABLE second profile (`SECOND`, spot, 2/8 precision) and
  asserts `build_history_source("SECOND", ..)` is `None`. This test's
  subject is deleted by this piece; section 6 replaces it with its
  inverse rather than removing it.
- `fills.rs::test_profiles` calls `set_boot_for_test`, and `run.rs` /
  `tape.rs` unit tests build `InstrumentProfiles` directly.
- `http.rs` `resolve_socket_symbol` unit tests, including
  `refusal_profiles(&["MNQ", "BTCUSDT"])` asserting the
  configured-but-not-booted refusal wording.
- `crates/mogwai-cli/tests/serving.rs`:
  `history_for_an_unserved_symbol_is_refused_with_400`,
  `ws_upgrade_refuses_an_unserved_symbol_with_400`,
  `a_symbol_no_preset_covers_is_served_under_the_default_bundle`.
- `scripts/smoke.py` asserts `/instruments` has exactly one entry equal
  to the readiness symbol. The smoke config configures ONE symbol, so
  both assertions stay true after this piece - including once
  `/instruments` answers from profiles rather than from
  `run.instrument`, which at one shape is byte-identical. They die at
  piece 13, not here. Verify before landing that the smoke config has no
  extra `[symbols.*]` table.

## 3. The target structure

All of it in `crates/mogwai-server/src/source.rs` unless stated. No new
module: every current caller already imports `source`, and a new file
would buy an import churn and nothing else.

### 3.1 Types

```rust
/// What identifies a river TODAY. A newtype rather than a bare `Symbol`
/// so piece 9 widens the key (resolved bundle + speed + generator-level
/// havoc) by changing this struct and the constructor, not every call
/// site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RiverKey(mogwai_protocol::Symbol);

impl RiverKey {
    pub(crate) fn for_symbol(symbol: &mogwai_protocol::Symbol) -> Self;
    pub(crate) fn symbol(&self) -> &str;
}

/// Everything that fixes WHICH tape a river realizes. Was `BootTape`
/// behind a `OnceLock`; now an owned value handed to `Rivers::new`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TapeIdentity {
    pub(crate) seeds: mogwai_protocol::RunSeeds,
    pub(crate) regime: Option<mogwai_protocol::MarketRegime>,
}

/// One deterministic path and the checkpoint chain that materializes it.
/// The mutex is PER RIVER: two rivers never serialize on each other.
struct River {
    checkpoints: Mutex<CheckpointIndex>,
}

/// The boatyard's river registry. One per serving process, owned by
/// `AppState`/`Run` rather than by a static, so a test may build two.
pub(crate) struct Rivers {
    identity: TapeIdentity,
    profiles: Arc<InstrumentProfiles>,
    rivers: Mutex<HashMap<RiverKey, Arc<River>>>,
}
```

`Rivers` is always held as `Arc<Rivers>`.

### 3.2 API

```rust
impl Rivers {
    pub(crate) fn new(identity: TapeIdentity, profiles: Arc<InstrumentProfiles>) -> Arc<Self>;
    pub(crate) fn profiles(&self) -> &InstrumentProfiles;
    pub(crate) fn identity(&self) -> TapeIdentity;

    /// The river for `symbol`, created on first use. `None` only when no
    /// profile resolves the symbol - which, until total resolution lands
    /// (piece 6/13), means "not configured".
    fn river(&self, symbol: &str) -> Option<Arc<River>>;

    // The six former free functions, now methods, same semantics:
    pub(crate) fn next_live_tick(&self, symbol: &str)
        -> Option<(Option<TickEvent>, Option<mogwai_data::TickFault>)>;
    pub(crate) fn activate_live(&self, symbol: &str) -> bool;
    pub(crate) fn arm_flow_surge(&self, symbol: &str, start_ns: u64,
        duration_ms: u64, rate_mult: f64, children_mult: f64) -> bool;
    pub(crate) fn clear_flow_surge(&self, symbol: &str) -> bool;
    /// `Ok(History::Unconfigured)` for a symbol no profile resolves;
    /// `Err` for a synthesis failure. Never conflates the two - see 3.4.
    pub(crate) fn history_source(&self, symbol: &str, start: Option<u64>)
        -> anyhow::Result<History>;
    pub(crate) fn last_trade_at_or_before(&self, symbol: &str, ts: u64)
        -> anyhow::Result<Option<Decimal>>;

    /// Generate and HOLD every instant up to `target_ns` on this river.
    /// The loop `materialize_warmup` used, factored out and reusable:
    /// bounded per acquisition by `MAX_EXTEND_TICKS` and in total by
    /// `MAX_WARMUP_MATERIALIZATION_TICKS`. Returns snapshots retained.
    pub(crate) fn ensure_reach(&self, symbol: &str, target_ns: u64)
        -> anyhow::Result<usize>;

    #[cfg(test)]
    pub(crate) fn river_handle_for_test(&self, symbol: &str) -> Option<Arc<River>>;
}
```

`materialize_warmup(symbol, profiles, boot, run_start_ns)` is deleted;
`serve.rs` calls `rivers.ensure_reach(boot_symbol, run_start_ns)`.
`build_live_source` (the `#[cfg(test)]` wrapper) becomes
`history_source(symbol, Some(sim_now))` at its one call site
(`http.rs` test) and is deleted.

### 3.3 Creation, exactly once, and the lock ordering

```rust
fn river(&self, symbol: &str) -> Option<Arc<River>> {
    let profile = self.profiles.get(symbol)?;
    let key = RiverKey::for_symbol(&profile.def.symbol);
    let mut rivers = self.rivers.lock().unwrap_or_else(PoisonError::into_inner);
    let river = Arc::clone(rivers.entry(key).or_insert_with(|| {
        Arc::new(River {
            checkpoints: Mutex::new(CheckpointIndex::new(
                generator(profile, self.identity),
                CHECKPOINT_K,
                MAX_EXTEND_TICKS,
            )),
        })
    }));
    drop(rivers);            // explicit: see the rule below
    Some(river)
}
```

Two rules, both load-bearing, both stated in the code as comments:

1. **Exactly-once creation.** `entry(..).or_insert_with` under the
   registry mutex is what makes two concurrent first-readers share ONE
   chain. A check-then-insert, or a `RwLock` upgrade dance, produces two
   chains for one river; passengers would then be on different water
   while believing they share it, and nothing downstream could detect it.
   `CheckpointIndex::new` does no walking (it clones a positioned
   generator at the origin), so holding the registry mutex across it
   costs microseconds.
2. **One lock ordering, and only one direction.** The registry mutex is
   RELEASED before any river mutex is taken. Never take the registry lock
   while holding a river lock. Every method follows the same shape: get
   the `Arc<River>`, drop the registry guard, then lock the river. This
   is the guard-scope family stated positively - the long work
   (`extend_toward`, a residual walk) is done under the RIVER's lock,
   owned by the task doing it, and the registry is never held across it.

### 3.4 `ensure_reach`, and the cold-river read

```rust
/// `symbol` must already have resolved to a river; the wrapper below is
/// what turns a symbol into one. Taking `&River` is not a style choice:
/// it makes it IMPOSSIBLE to re-enter the registry lock from a path that
/// may already hold a river lock (rule 2 of 3.3).
fn reach_river(river: &River, target_ns: u64) -> anyhow::Result<usize> {
    let mut walked_total = 0usize;
    loop {
        let (extended, frontier_ns, checkpoints) = {
            let mut guard = locked(&river.checkpoints);
            // Returns None when the river is live: the live check and the
            // extension are ONE operation under ONE guard. See below.
            let extended = guard.extend_toward_unless_live(target_ns);
            (extended, guard.frontier_ns(), guard.checkpoint_count())
        };
        let Some(walked) = extended else {
            // The paced worker owns the lead. Not an error: report what is
            // already reachable and let the caller's positioning refuse if
            // the frontier is short.
            return Ok(checkpoints);
        };
        walked_total = walked_total.saturating_add(walked);
        anyhow::ensure!(walked_total <= MAX_WARMUP_MATERIALIZATION_TICKS, ...);
        if frontier_ns >= target_ns { return Ok(checkpoints); }
        anyhow::ensure!(walked > 0, "generator stopped before {target_ns}");
    }
}

pub(crate) fn ensure_reach(&self, symbol: &str, target_ns: u64) -> anyhow::Result<usize> {
    let river = self.river(symbol)
        .with_context(|| format!("symbol {symbol} has no configured shape"))?;
    reach_river(&river, target_ns)
}
```

Byte-for-byte the current `materialize_warmup` body, minus the `BOOT.set`
and with the chain reached through the registry, plus the live guard.
`history_source` calls the reach FIRST, so a cold river answers a
distant instant instead of returning `None` after one bounded extension.

**The live check must be atomic with the extension.** `extend_toward`
does NOT consult `live` - only `try_source_before_target` does, and it
consults it to decide whether to CALL `extend_toward`. So an
`if !is_live() { ensure_reach(..) }` in the caller is a check-then-act
race: `activate_live` can land in the window between the two, and a
history reader then advances a live river's frontier out from under the
paced worker that is supposed to be its sole owner. It is also the
guard-scope family - the decision and the work it authorizes are under
different acquisitions of the same lock. The fix is in `mogwai-data`, in
`crates/mogwai-data/src/generated/checkpoint.rs`, and it REPLACES the
`is_live` accessor step 1 was going to add (an accessor is precisely the
shape that invites the race back):

```rust
/// Walk toward `target`, or refuse when the paced worker owns the lead.
/// `None` means "this river is live"; `Some(n)` the ticks walked. The
/// check is inside the `&mut self` borrow, so no caller can observe a
/// not-live index and then extend a live one.
pub fn extend_toward_unless_live(&mut self, target: u64) -> Option<usize> {
    (!self.live).then(|| self.extend_toward(target))
}
```

`extend_toward` itself stays `pub` and unchanged (the paced worker and
`try_source_before_target` are its legitimate callers); every path that
is not the lead's owner goes through the checked form.

**`history_source` returns a `Result`, not an `Option`.** Swallowing the
reach failure with `.ok()?` would report a blown ceiling, a stopped
generator or a poisoned walk as `None`, and `bounded_trades` /
`bounded_quotes` render `None` as an empty vector - so an internal
synthesis failure would leave the venue answering `200 []`,
indistinguishable from a genuinely empty window. That is the silent
failure this whole piece exists to delete, reintroduced one layer down.
An unknown symbol stays a distinct, non-error outcome, because it is the
400 the handler already produces:

```rust
pub(crate) enum History { Unconfigured, Source(Box<dyn TickSource>) }

pub(crate) fn history_source(&self, symbol: &str, start: Option<u64>)
    -> anyhow::Result<History> {
    let target = start.unwrap_or(TAPE_ORIGIN_NS);
    let Some(river) = self.river(symbol) else { return Ok(History::Unconfigured) };
    reach_river(&river, target)?;
    let mut guard = locked(&river.checkpoints);
    let Some(positioned) = guard.try_source_at_or_before(target) else {
        // Reachable without an error only on a live river whose frontier
        // is still short of the target - a legitimate "not yet".
        anyhow::bail!("river {symbol} cannot reach {target}");
    };
    drop(guard);
    Ok(History::Source(Box::new(MergeSource::starting_at(vec![Box::new(positioned)], start))))
}
```

`bounded_trades` / `bounded_quotes` return `anyhow::Result<Vec<_>>` and
their handlers map `Unconfigured` to the existing 400 (wording
unchanged, `serving.rs::history_for_an_unserved_symbol_is_refused_with_400`
pins it) and `Err` to a 500 naming the symbol and the target. `fills.rs`
and `tape.rs` callers propagate rather than `?`-into-`None`; where a
caller genuinely has nothing to do with an error - the sweeper's
per-symbol pass - it logs at `warn` and skips that symbol, which is
still not silence.

**The reach ceiling is a PER-CALL work bound, and that is now explicit.**
`walked_total` is local to `reach_river`, so successive calls can walk a
cold river past `MAX_WARMUP_MATERIALIZATION_TICKS` cumulatively.
`extend_toward` is monotonic - a later, further target does only the new
delta - so the sum over calls is bounded by the reach the run clock
allows anyway, and no absolute per-river cap is wanted (one would wedge
a long run's history at an arbitrary instant, the frontier-family
failure in its fence form). What is required is that the constant stop
claiming otherwise: its doc comment says "total legitimate boot reach",
which was true of a once-per-process warmup and is false of a
per-request bound. Rewrite it in step 2 as the per-acquisition work
bound it now is, and say WHY there is no absolute cap. This is one of
the comments section 3.9 governs.

**First-touch cost on a cold river, stated rather than waved off.** The
ceiling argument above ("bounded by the same run clock") is a bound on
the WORK, not on the LATENCY of the request that pays for it. The first
`/trades?symbol=MNQ` at run-time T pays the entire materialization from
`TAPE_ORIGIN_NS` to T - the same walk `serve.rs` deliberately does
before announcing readiness - synchronously, inside one `spawn_blocking`
slot, holding that river's mutex, under the client's HTTP timeout. Every
later request on that river is cheap; the first is not. Memory is the
same shape: `MAX_CHECKPOINTS` generator clones are a hard ceiling PER
RIVER, so N touched rivers multiply the resident footprint by N.

This piece accepts both and does NOT warm every configured river at
boot: warming N rivers would multiply time-to-readiness by N for a
capability most runs never touch, and the boat placement that would
justify pre-warming is piece 9's. The obligation here is that the cost
is documented at `ensure_reach` and in `docs/config.md`'s new paragraph
(3.9) - a cold non-boot river's first history request is slow and
allocates - rather than discovered. If a run needs the cost paid up
front, the lever is a boot-time `ensure_reach` per configured symbol,
and it is deliberately not wired here.

The `MergeSource`-around-one-source and the inclusive-`start` contract
documented on today's `build_history_source` move to `history_source`
UNCHANGED, comment included; the sweeper's `from_ns + 1` still depends on
inclusivity.

Reach is not unbounded in practice: `http.rs` already refuses a history
`start` above the run clock's now, and the sweeper walks only up to
`to_ns`, so a cold river's target is bounded by the same clock the boot
river's warmup was bounded by, under the same ceiling constant.

### 3.5 Lazy engine registration

`crates/mogwai-engine/src/lib.rs` gains:

```rust
/// Register an instrument the engine has not seen. Returns `true` when
/// this call added it. Idempotent and NON-DESTRUCTIVE: an already
/// registered symbol keeps its def, its margin policy, its fee schedule
/// and every open order - re-registration must never reset venue state
/// for a symbol a client is already trading.
pub fn ensure_instrument(&mut self, def: InstrumentDef) -> bool {
    match self.instruments.entry(Arc::clone(&def.symbol)) {
        Entry::Occupied(_) => false,
        Entry::Vacant(slot) => { slot.insert(def); true }
    }
}
```

`EngineConfig::instruments` STAYS (the engine's own tests and
`Engine::unbound` use it, and a host constructing an engine with a known
set is legitimate). `Run::new` passes an EMPTY vector and registers
through the new server-side path.

Server side, `run.rs`:

```rust
/// Make `symbol` tradable on this run's engine: register the def and
/// install the margin policy and fee schedule from its profile. Called
/// when a socket binds a symbol and before an order for it is admitted.
/// Returns `false` when no profile resolves it.
pub(crate) async fn ensure_instrument(&self, symbol: &str) -> bool;
```

It resolves `self.rivers.profiles().get(symbol)`, locks the engine
(`AsyncMutex`), calls `Engine::ensure_instrument`, and - only when that
returned `true` - calls `set_margin_policy` / `set_fee_schedule` from the
profile. Guarding the policy installs on the `true` return is what keeps
a re-bind from resetting a live symbol's configuration.

The margin/fee installation code moves out of `Run::new` into this method
verbatim (the `BreachAction` and `FeeRate` conversions included), so
there is ONE path that turns a profile into engine policy rather than
two that can drift.

Call sites:

- `ws.rs`, immediately after `resolve_socket_symbol` succeeds and before
  the socket is bound. A failure here is the same 400 as an unresolvable
  symbol.
- `http::process_order_cmd` - and this is a NEW call site, not a
  rewrite of an existing check. An earlier draft of this spec said the
  order path "already checks
  `state.profiles.get(&order.symbol).is_some()`" and that the check
  simply becomes `ensure_instrument`. It does not: that predicate is the
  third conjunct of the special rejection for a CONFIGURED, price-less
  MARKET order (the venue must not blame the client for its own failure
  to synthesize a price). Overloading it would register only market
  orders, would register AFTER market synthesis, and would silently
  change what that condition means. Leave it exactly as it is - it keeps
  reading `state.rivers.profiles().get(..)` and nothing more.

  The registration goes in as its own statement in `process_order_cmd`,
  AFTER the bound-symbol comparison and BEFORE the act delay and any
  market reading: `if !state.run.ensure_instrument(&order.symbol).await
  { /* fall through to the engine's unknown-instrument rejection */ }`.
  Falling through rather than short-circuiting is deliberate: an
  unconfigured symbol must still produce the engine's existing "unknown
  instrument" rejection with its existing wording, not a new one. The
  bound-symbol comparison that
  produces "does not match the symbol this connection is bound to" is
  UNCHANGED and still runs first (`serving.rs::an_order_for_another_symbol_is_refused_on_a_bound_socket`
  pins its wording).

### 3.6 Ownership and threading

- `Rivers` is constructed in `serve_async` right after
  `build_instrument_profiles` and the seed derivation, and is stored in
  `Run` as `pub(crate) rivers: Arc<Rivers>` and in `AppState` as
  `rivers: Arc<Rivers>`. `AppState::profiles` and `Run::instrument` are
  DELETED; `state.rivers.profiles()` replaces the former, and the two
  readers of the latter are handled in 3.7.
- `TapeSpawn.profiles` becomes `TapeSpawn.rivers: Arc<Rivers>`, and
  `Tape` stores `rivers: Arc<Rivers>` beside its `symbol`. Every
  `source::foo(&symbol, &profiles)` in `tape.rs` becomes
  `self.rivers.foo(&symbol)` / `spawn.rivers.foo(&symbol)`.
- `FillSweep.profiles` becomes `FillSweep.rivers`; `fills::scan_triggers`,
  `scan_triggers_with_budget`, `read_market`, `read_last` and
  `MarketReadingCache::read` take `&Rivers` instead of
  `&InstrumentProfiles` (they need both the chain and, in
  `read_market`'s case, `profiles.get(symbol)?.def.price_increment`,
  which `rivers.profiles()` still gives them).
- `http::bounded_trades` / `bounded_quotes` take `&Rivers`, and the
  `Arc::clone(&state.profiles)` their handlers move into `spawn_blocking`
  becomes `Arc::clone(&state.rivers)`. The clone must be taken OUTSIDE
  the closure and moved in, as today: the blocking task outlives the
  handler future, which hyper drops on client disconnect, so borrowing
  `state` into it is the guard-scope failure in its canonical form.
- `fill_golden.rs` builds its own `Rivers` from its fixed identity.

### 3.7 The two `run.instrument` readers

- `http::instruments` becomes
  `Json(state.rivers.profiles().instrument_defs())` - already sorted by
  symbol. With one configured shape this is byte-identical to today's
  answer, so `scripts/smoke.py` and `serving.rs` stay green. The
  endpoint's DOC contract (piece 13) is not written here; the code change
  is forced by deleting the field and is not deferrable.
- `ws.rs` passes `bound` to `resolve_socket_symbol`. `Run` gains
  `pub(crate) boot_symbol: mogwai_protocol::Symbol` - the boot shape's
  symbol, which under piece 4's ruling still exists - and ws.rs passes
  that. This is a rename of the surviving half of `run.instrument`, not
  a new concept.

### 3.8 `build_instrument_profiles` stops collapsing

`config.rs`:

```rust
pub fn build_instrument_profiles(cfg: &Config) -> anyhow::Result<source::InstrumentProfiles> {
    validate_symbol_keys(cfg)?;
    let mut resolved = Vec::new();
    // unchanged sweep order: boot shape first, then configured keys sorted
    for symbol in std::iter::once(cfg.boot_symbol()).chain(configured.into_iter().map(Some)) {
        let named = symbol.unwrap_or(DEFAULT_PRESET);
        let profile = profile_for(cfg, symbol).with_context(...)?;
        refuse_unfunded_settlement(cfg, &profile.def).with_context(...)?;
        resolved.push(profile);
    }
    Ok(source::InstrumentProfiles::from_profiles(resolved))
}
```

The sweep order, the two `with_context` messages and
`refuse_unfunded_settlement` are untouched - the boot check stays a boot
check, per the settled funding bullet, and now covers exactly the set
`/instruments` reports. `InstrumentProfiles::from_profiles` already keys
by `def.symbol`, so a `[symbols.X]` table whose resolved def carries the
boot symbol collapses onto it, which is the pre-existing behaviour for
one entry generalized. `serve.rs` replaces the `.next()` with a lookup of
the boot symbol:

```rust
let boot_symbol = profiles.boot_symbol_def(cfg.boot_symbol())?;
```

- a new `InstrumentProfiles` method returning the def for the named
symbol, erroring by name when nothing resolves it. Its rule for a `None`
argument is EXACTLY `build_instrument_profiles`'s own -
`symbol.unwrap_or(DEFAULT_PRESET)`, i.e. look up `BTCUSDT` - and NOT
"the sole entry". Once profiles are plural there need not be a sole
entry: no top-level `symbol` plus a `[symbols.MNQ]` table yields two, and
a sole-entry rule would then either panic or pick the wrong boot shape.
Sharing the one expression with the sweep is what keeps the def
`serve.rs` readies identical to the def the sweep swept first.

`gen.rs` and `walk.rs` (2.2b) call the same method with their config's
`boot_symbol()` in place of their `expected exactly one` destructure.

`ReadyRecord.symbol` keeps taking that value; its schema is piece 12's.

One note on the collapse remark above: it is nearly dead code. The sweep
already filters `[symbols.*]` keys that case-match the boot symbol, and
`validate_symbol_keys` refuses two keys differing only in case, so the
only way a non-boot key resolves onto the boot symbol is a key equal to
`DEFAULT_PRESET` under a config with no top-level `symbol`. Keep the
`from_profiles` keying as stated; do not build any dedup logic for a
case the sweep has already narrowed to one.

### 3.9 Comments that must be corrected in the same landing

These are durable prose asserting a live type, and the type dies here:

- `http.rs::resolve_socket_symbol`'s doc block ("TOTAL resolution ... is
  NOT reachable yet: `source::RunIndex` is a process-global holding one
  symbol"). Rewrite: the registry is keyed now, so a configured symbol
  RESOLVES; what a `/ws` socket still cannot have is its own BOAT, since
  one paced `Tape` is placed at boot and the boatyard is piece 9. The
  refusal therefore stays and its wording ("configured but is not the
  river this run booted") becomes exactly true rather than a stand-in.
- `http.rs`'s "other symbols' requests do not queue here at all" - true
  after this piece, since per-river mutexes replace the one global
  index. Restate it as a consequence of the per-river lock instead of an
  assertion about there being no other symbols.
- `run.rs`'s module comment "There are deliberately no keys or lookup
  methods here: a process is one instrument, one ledger, and one tape."
  Becomes: one ledger and one boat, many rivers.
- `source.rs`'s `RunIndex` doc comment is deleted with the type.
- `MAX_WARMUP_MATERIALIZATION_TICKS`'s doc comment ("total legitimate
  boot reach") describes a once-per-process warmup budget and becomes
  false the moment the constant bounds a per-request reach. Rewrite it as
  the per-acquisition work bound it is, saying why there is no absolute
  per-river cap (3.4).
- `http.rs`'s `build_instrument_profiles keeps only the boot profile, so
  a symbol that ...` comment above `resolve_socket_symbol` is falsified
  by step 3 and must land corrected in the same commit.
- `fills.rs`'s `TapeKey`/`MarketRegime` stale comment and the two dead
  `current_price` references in `http.rs` are the SAME family and are in
  the files this piece rewrites; fix them here rather than leaving a
  known-false comment in a file we just touched.
- `docs/config.md`'s "The top-level `symbol` is the one symbol this slice
  boots" and "the one it serves and every `[symbols.*]` shape" (which
  implies the others are validated but not served) need one paragraph:
  every configured shape is now servable for history and reported by
  `/instruments`; the live paced tape is still the boot shape's. Same for
  `reference/architecture.md` wherever it states one process is one
  checkpoint chain - grep `RunIndex`, "process global", "one instrument"
  across `docs/` and `reference/` before landing and fix every hit.

Prose is written WITH this change, per the standing item, not afterwards.

## 4. Landing order

Each numbered step is one commit that compiles and leaves the suite
green. There is no partial state where the venue serves from two
mechanisms.

1. **`CheckpointIndex::extend_toward_unless_live` +
   `Engine::ensure_instrument`.** Two
   additive primitives in `mogwai-data` and `mogwai-engine`, with their
   unit tests. Nothing calls them yet.
2. **The registry, whole.** In one commit: `RiverKey`, `TapeIdentity`,
   `River`, `Rivers` and its methods replace `INDEX`, `BOOT`, `RunIndex`,
   `set_boot_for_test`, `materialize_warmup` and the six free functions;
   `tape.rs`, `fills.rs`, `sweeper.rs`, `http.rs`, `fill_golden.rs`,
   `run.rs` and `serve.rs` move to the handle. This is the intrusive
   change and it is not splittable - the statics cannot half-exist.
   `build_instrument_profiles` still returns one profile here, so
   behaviour is unchanged and every existing test must pass untouched
   EXCEPT the ones in 6.1.
3. **Plural profiles.** `build_instrument_profiles` keeps every shape,
   `serve.rs` resolves the boot def by name, `gen.rs` and
   `fit/walk.rs` drop their `expected exactly one` destructure for the
   same by-name lookup (2.2b) IN THIS COMMIT, `/instruments` answers from
   profiles, `Run::instrument` becomes `Run::boot_symbol`. This is the
   commit that changes observable behaviour, and it is the keep/revert
   unit: revert it and the venue is single-shape again on top of a
   perfectly good registry.
4. **Lazy engine registration.** `Run::new` builds an empty engine,
   `Run::ensure_instrument` owns policy installation, ws.rs and the order
   path call it.
5. **Prose.** Bundled into 3 and 4 per the commit rules, not a separate
   landing.

Steps 2 and 3 could be merged; they are kept apart so the mechanism
change and the behaviour change have separate revert verdicts.

## 5. Determinism and the tape version

**No `TAPE_PROTOCOL_VERSION` bump is owed by this piece**, and that is a
claim to check rather than assume:

- Seed derivation is untouched. `RunSeeds::from_run_seed` still has no
  symbol term - that is piece 8, and it is what makes two configured
  symbols with identical scalars produce IDENTICAL prints today. Section
  6's tests must not assert that two symbols differ, because they do not.
- `generator()` builds each river with the same arguments it builds today
  (the profile's scalars/session/grid/calendar, `identity.seeds.tape`,
  `TAPE_ORIGIN_NS`, `identity.regime`), read from an owned value instead
  of a static.
- `CHECKPOINT_K`, `MAX_EXTEND_TICKS` and
  `MAX_WARMUP_MATERIALIZATION_TICKS` keep their values. Per-river rather
  than per-process changes nothing for a one-river run.
- No committed artifact is regenerated.

The gate that proves it: `two_runs_with_the_same_configured_seed_serve_the_same_first_trades`
in `serving.rs` and the `mogwai-server` golden fill transcript, both
unchanged and both re-run. If either moves, this piece changed the tape
and the bump is owed - stop and re-derive rather than re-bless.

## 6. Verification, per brick

Every command copy-pasteable. Bite-check each new regression test by
reverting the production line as a TEXT EDIT and restoring it the same
way - never `git checkout --`.

### 6.1 Tests deleted or inverted

- `fills.rs::initialized_run_index_refuses_every_other_symbol` is
  REPLACED, keeping its fixture (the fully-resolvable `SECOND` profile),
  by `every_configured_symbol_gets_its_own_chain`:
  `rivers.history_source("BTCUSDT", Some(TEST_ORIGIN)).is_some()` AND
  `rivers.history_source("SECOND", Some(TEST_ORIGIN)).is_some()`, while
  `rivers.history_source("NOT-A-SYMBOL", ..).is_none()` still holds. The
  original comment's warning survives inverted: the second symbol is
  fully resolvable, so a pass here means the registry really keyed the
  chain rather than the lookup vacuously succeeding.
- `source::set_boot_for_test` and its collision assertion are deleted;
  `fills::test_profiles` returns profiles and a new
  `fills::test_rivers()` returns `Rivers::new(TapeIdentity { seeds:
  RunSeeds::from_run_seed(42), regime: None }, ..)`. A test wanting a
  different seed may now simply build another `Rivers`, which is the
  point.

### 6.2 New tests

Unit, `mogwai-data`:

- `a_live_index_refuses_to_be_extended` -
  `extend_toward_unless_live` returns `Some(n)` at construction and
  `None` after `activate_live`, and the frontier is UNMOVED across the
  refused call. Asserting on the frontier, not only on the `None`, is
  what distinguishes a real refusal from a bound checked after the walk.

Unit, `mogwai-engine`:

- `ensure_instrument_is_idempotent_and_preserves_policy` - register a
  def, set a margin policy and a fee schedule, `ensure_instrument` the
  same symbol again, assert `false` returned and both policies intact.

Unit, `mogwai-server`:

- `every_configured_symbol_gets_its_own_chain` (6.1).
- `concurrent_first_readers_share_one_river` - 8 threads call
  `river_handle_for_test("BTCUSDT")`; assert all 8 `Arc::ptr_eq`. Bite:
  swap `entry().or_insert_with` for a get-then-insert and it fails.
- `a_second_river_answers_on_its_own_price_grid` - read history for
  `SECOND` (spot, `price_increment` 0.01) and assert every trade price is
  a multiple of its own increment, not BTCUSDT's. This is the assertion
  that survives the shared seed: same draws, different grid.
- `a_cold_river_reaches_an_instant_far_past_one_extension` -
  `history_source` on a never-warmed river at a target beyond one
  `extend_toward` returns `Some`. Bite: delete the `ensure_reach` call in
  `history_source` and it returns `None`.
- `the_live_river_is_not_extended_by_a_reader` - `activate_live`, then
  `history_source` past the frontier errors rather than walking, and the
  frontier is unmoved. This pins the sequential case: the paced worker
  stays the sole owner of the lead.
- `activation_racing_a_cold_reach_never_moves_the_live_frontier` - the
  race the sequential test above CANNOT catch, and the reason
  `extend_toward_unless_live` exists rather than an `is_live` accessor.
  N iterations, each: a fresh cold river, one thread calling
  `history_source` at a far target while another calls `activate_live`;
  after both join, assert the frontier is either short (the reader lost)
  or reached (the reader won) but NEVER advanced after the activation -
  record the frontier at activation time inside the index and assert no
  growth past it. Bite: restore the caller-side
  `if !is_live() { ensure_reach(..) }` shape and, under `-N 50`, the
  assertion fires.
- `a_reach_failure_is_an_error_not_an_empty_window` - drive a river whose
  generator stops (or whose target exceeds
  `MAX_WARMUP_MATERIALIZATION_TICKS` in one call) and assert
  `history_source` returns `Err`, and that `bounded_trades` propagates it
  rather than returning an empty vector. Bite: swap the `?` back for
  `.ok()?` and the test sees `Ok(vec![])`.
- `a_boot_symbol_that_is_not_first_alphabetically_is_the_one_readied` -
  config with `symbol = "MNQ"` and a `[symbols.BTCUSDT]` table;
  `build_instrument_profiles` returns both and
  `boot_symbol_def` answers MNQ. Bite: restore `.next()` over
  `instrument_defs()` (sorted) and it answers BTCUSDT.
- `an_unset_boot_symbol_resolves_the_default_shape_among_several` - no
  top-level `symbol`, plus a `[symbols.MNQ]` table.
  `build_instrument_profiles` returns two and `boot_symbol_def(None)`
  answers BTCUSDT. Bite: implement the rule as "the sole entry" and it
  panics or answers MNQ. This is the case section 3.8's earlier
  sole-entry wording got wrong.

Unit, `mogwai-cli` and `mogwai-lab`:

- `a_scratch_config_with_a_second_symbol_table_still_resolves_its_boot_shape`
  - one test per crate over `gen.rs` and `fit/walk.rs`, on a config
  carrying a boot shape plus one `[symbols.*]` table. Bite: keep the
  `expected exactly one` destructure and it bails.

Integration, `crates/mogwai-cli/tests/serving.rs`, with a new
`tests/configs/two-symbols.toml` (`seed = 42`, `warmup_ns` as in
`fast.toml`, `speed = 0.0`, `symbol = "BTCUSDT"`, plus a
`[symbols.MNQ]`-shaped table resolved from the MNQ preset and funded by
the default balances - confirm MNQ's settlement currency is in
`[balances]` or add it, since `refuse_unfunded_settlement` now guards a
shape that is actually reachable):

- `history_is_served_for_a_configured_symbol_that_is_not_the_boot_river`
  - `/trades?symbol=MNQ&start=0&limit=5` returns 200 with MNQ-symbol
  rows. Bite: revert `build_instrument_profiles` to `get_or_insert` and
  it 400s.
- `instruments_reports_every_configured_shape` - `/instruments` returns
  both, sorted.
- `a_ws_upgrade_for_a_configured_non_boot_symbol_is_refused_naming_the_boat`
  - pins the surviving compatibility restriction so piece 9 knows
  exactly what it deletes.
- `history_for_an_unserved_symbol_is_refused_with_400` (existing) must
  still pass against the two-symbol venue: its refusal body now lists
  both served symbols.

### 6.3 Commands

```
brokkr check
brokkr test -p mogwai-data a_live_index_refuses_to_be_extended
brokkr test -p mogwai-engine ensure_instrument_is_idempotent_and_preserves_policy
brokkr test -p mogwai-server every_configured_symbol_gets_its_own_chain
brokkr test -p mogwai-server concurrent_first_readers_share_one_river -N 50
brokkr test -p mogwai-server a_second_river_answers_on_its_own_price_grid
brokkr test -p mogwai-server a_cold_river_reaches_an_instant_far_past_one_extension
brokkr test -p mogwai-server the_live_river_is_not_extended_by_a_reader
brokkr test -p mogwai-server activation_racing_a_cold_reach_never_moves_the_live_frontier -N 50
brokkr test -p mogwai-server a_reach_failure_is_an_error_not_an_empty_window
brokkr test -p mogwai-server an_unset_boot_symbol_resolves_the_default_shape_among_several
brokkr test -p mogwai-cli a_scratch_config_with_a_second_symbol_table_still_resolves_its_boot_shape
brokkr test -p mogwai-lab a_scratch_config_with_a_second_symbol_table_still_resolves_its_boot_shape
brokkr test -p mogwai-cli history_is_served_for_a_configured_symbol_that_is_not_the_boot_river
brokkr test -p mogwai-cli instruments_reports_every_configured_shape
brokkr test -p mogwai-cli two_runs_with_the_same_configured_seed_serve_the_same_first_trades
brokkr test -p mogwai-server fill_golden
```

Live end-to-end, for the execution and engine semantics the lazy
registration touches:

```
brokkr run mogwai -- serve
python3 scripts/smoke.py
```

The smoke test is the gate that proves lazy registration did not break
order handling: with no instrument in the engine at boot, a run whose
first order arrives before `ensure_instrument` would reject it, and smoke
submits orders over `/ws` end to end.

Adapter gate: this piece touches no adapter file, so `brokkr check
--gate` is not required. Run it anyway before the landing commit if any
`mogwai-protocol` type moved - none should.

No measurement instrument is owed: this spec claims capability, not
throughput. The one performance CLAIM it makes (per-river locks stop
unrelated symbols serializing) is structural and is stated as a comment
correction, not as a benchmarked win; do not add a bench for it.

## 7. Stopping rule

The teardown stops at the boundary of `mogwai-server`'s tape-state
ownership plus the two additive primitives in `mogwai-data` and
`mogwai-engine`, plus ONE forced edit each in `mogwai-cli`'s `gen.rs` and
`mogwai-lab`'s `fit/walk.rs`: the `expected exactly one` destructure that
step 3 falsifies (2.2b). That is the whole of this piece's reach into
`mogwai-lab`, and it is a call-site repair, not a change to the fit
method. It does not touch: the generator, the fingerprint, seed
derivation, the wire protocol, the adapter, the rest of `mogwai-lab`, or
any committed artifact.

## 8. Excluded, by name, with their owner

Each is a separate piece in `notes/todo.md`, not deferral:

- **Piece 6** - the `/ws` symbol carrier and admission tickets. A
  `?symbol=` query already exists on the upgrade; the subscribe mechanism
  does not, and this piece adds none. The non-boot-symbol `/ws` refusal
  stays and is pinned by a test.
- **Piece 8** - the symbol dimension in seed derivation, and the
  `TAPE_PROTOCOL_VERSION` bump it owes. Until it lands, two configured
  symbols with identical scalars realize identical prints; section 6's
  tests are written not to depend on the difference.
- **Piece 9** - the boatyard: the sharing key inside `RiverKey`, a paced
  `Tape` per boat, idle-river retirement, ring topology. This piece
  places exactly one boat, at boot, on the boot river.
- **Piece 10** - one clock per boat. `Run::sim`, `/clock`,
  `run_duration_ns` and `AccountState` stamps stay run-level here.
- **Piece 11** - `MarketReadingCache`'s single entry behind one mutex and
  the process-wide `last_swept_ns`. Both survive untouched. They are safe
  under this piece because only the boot symbol can be traded (the `/ws`
  bind restriction), so the one-entry cache is never thrashed and the
  shared settlement watermark covers one symbol.
- **Piece 12** - `ReadyRecord`'s schema. `symbol` is still written, from
  the boot shape.
- **Piece 13** - the consumer surface: the `/instruments` DOC contract,
  the adapter's subscription guard, `scripts/smoke.py`'s one-instrument
  assertions, and broadarrow's `mogwai_facts`. The code change to
  `/instruments` is forced here; the contract prose and the consumer
  breakage belong there, and a one-shape config keeps every existing
  consumer assertion true in the meantime.

## 9. Review findings folded in, and the ones rejected

Two independent reviews of this spec (a Claude pass and a codex deep
pass) are consolidated above. Both found the live-frontier race and the
reach-ceiling ambiguity independently, which is why those got the
heaviest treatment. Folded in: the out-of-server
`build_instrument_profiles` callers (2.2b, 3.8, 6.2, 7), the
`boot_symbol_def` rule for an unset boot symbol (3.8), the check-then-act
race on `live` and its replacement primitive (3.4, 6.2), the
`ensure_reach`/registry-lock re-entrancy (3.4's `&River` form), the
`.ok()?` that turned synthesis failures into `200 []` (3.4), the
first-touch latency and per-river memory (3.4), the order-path call site
that does not exist in the claimed form (3.5), the ceiling constant's now
false doc comment (3.9), the `spawn_blocking` clone (3.6), and the
removal of the cited line numbers.

REJECTED, with the reason:

- **"Non-preset `[symbols.X]` keys are not servable: `bundle_name` falls
  back to `DEFAULT_PRESET`, so the resolved `def.symbol` is BTCUSDT and
  the tables silently collapse onto the boot entry."** False.
  `bundle_name` selects the BUNDLE, not the symbol;
  `resolve_instrument_named` re-inserts the REQUESTED symbol into the
  merged table as its last act before returning, so `[symbols.FOO]`
  resolves a def whose `symbol` is `FOO`, served under the default
  bundle's dynamics. That is the shipped, tested behaviour -
  `serving.rs::a_symbol_no_preset_covers_is_served_under_the_default_bundle`
  pins exactly it, and it is the AGENTS.md rule that the instrument set
  is open. No silent collapse, no named refusal owed, no test owed. The
  narrow residue of the collapse question is recorded at the end of 3.8.
- **"Add a bench for the per-river lock claim."** Neither review asked
  for this, and section 6's closing paragraph already forecloses it; it
  is restated here because the claim reads like a performance win and
  invites one. No decision would change on the result.
