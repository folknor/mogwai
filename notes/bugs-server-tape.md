# Bug hunt: mogwai-server tape and fill path

Scope: `source.rs`, `tape.rs`, `gen.rs`, `fills.rs`, `fill_golden.rs`,
`tick_composition.rs`, `sweeper.rs`.

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Cross-scope: findings 2 and 6 confirm and extend `bugs-data.md` findings 2 and 1.

## 1. The sweeper runs two blocking tape walks directly on a tokio worker (confirmed, high)

`sweeper.rs`. Phase two and three of every pass call, inline in the async task:

- `sweep.market_readings.read(...)` - on a cache miss, `fills::read_market` -
  checkpoint restore plus a 300 s window walk. The crate's own instrument
  (`fills.rs`) measures this at ~12.6 ms median.
- `fills::read_last(symbol, to_ns, ...)` - `source::last_trade_at_or_before` -
  checkpoint restore plus a residual replay bounded only by `CHECKPOINT_K` =
  4,194,304 ticks (~1.4 s at the documented 2.9M ticks/s).
- The same `read_last` again per settlement instant.

`fills.rs` states as contract, twice: "Synchronous and CPU-bound; callers run it on
`spawn_blocking`." `sweeper.rs` repeats it: "the tape walk ... runs OFF the engine
lock and on `spawn_blocking` or it stalls both order entry and a runtime worker."
Only the `scan_triggers` call honours it. The mark/settlement half does not.

Failure scenario: a futures run at the default 100 ms interval stalls a runtime
worker for tens of ms every pass, and up to ~1.4 s whenever `read_last`'s residual is
long (right after boot, or after any period with no history reads to advance the
shared lead). Under acceleration `MIN_SWEEP_WALL` puts the pass every 5 ms wall, so
the worker is essentially never released. This also holds the process-global `INDEX`
mutex for the extend portion, so `/trades`, `/quotes` and submits queue behind it.

Additionally `MarketReadingCache::read` uses `.expect("market reading cache
poisoned")` where every other lock site in the crate uses `PoisonError::into_inner`.
One panic inside `read_market` converts a transient fault into a permanently wedged
submit and sweep path.

## 2. `BoundedSeek` is gone and nothing replaced it - the hang guard is now fictional

`BoundedSeek` exists nowhere in the repo. Three durable comments still claim it is
load-bearing:

- `fills.rs` - "`BoundedSeek` caps only its `seek_to`; its `next_tick` delegates
  uncapped..."
- `checkpoint.rs` - "A target past this bound leaves the frontier short; the caller's
  own `BoundedSeek` then caps too and the seek yields an empty page instead of
  hanging."
- `checkpoint.rs` again, and `reference/performance.md`.

That second one is now false and it is the safety argument for `MAX_EXTEND_TICKS`.
Actual path today: `build_history_source` -> `source_at_or_before(start)` ->
`extend_toward` walks at most `1<<30` ticks and returns a checkpoint short of `start`
-> `MergeSource::starting_at` calls the trait-default `seek_to`, which loops
`next_tick` until `ts_event >= start`. `GeneratedSource::next_tick` never returns
`None`. So a target more than 2^30 ticks past the frontier is an infinite loop, not
an empty page.

Reachability: `/trades` and `/quotes` clamp `start <= sim_now`, which is the only
reason this is currently latent. It becomes live if (a) the shared lead falls more
than 2^30 ticks behind sim-now - nothing advances the lead except history/fill reads,
since the tape worker walks an independent clone - or (b) any future caller passes an
unclamped instant. The hang would consume a `spawn_blocking` thread permanently, one
per request.

`checkpoint.rs` also still says "~34M ticks at the server's K = 8192". K is
4,194,304, 512x larger.

## 3. `CHECKPOINT_K`'s stated rationale is incoherent, and the constant is now the dominant cost driver

`source.rs` justifies raising the stride to 4,194,304 by pointing at "budget
ceilings" and p99.9 expansion, while the same comment says the per-request seek
budget no longer exists. But the stride's only cost function is the residual replay:
a larger K means every restore replays more ticks. There is nothing left that the
large K buys. It is what makes `read_market` cost 12.6 ms per miss and `read_last`
cost up to 1.4 s. The comment reasons about a budget as if it were still capping the
residual, when in fact the residual is now unbounded-in-practice latency paid by the
submit path and the sweeper.

Worse, the residual replay goes through `MergeSource::starting_at`'s default
`seek_to`, which calls `next_tick` - and `GeneratedSource` does not override
`seek_to`. So every skipped tick materializes a full `TradeTick`/`QuoteTick`: a
`Symbol` String clone, two `Decimal`s, an `Option<QuoteTick>` shuffle - all
discarded. `GeneratedSource::advance_parent` exists precisely to advance without
materializing, and `tick_composition.rs` proves it is bit-identical to the wire walk.
A `seek_to` override on `GeneratedSource` that uses `advance_parent` for whole parents
and only materializes the tail parent would cut the restore cost by a large multiple,
for a few dozen lines. This is the single highest-leverage fix in scope.

## 4. Structural: the fill path re-synthesizes a tape the process is already walking

Right now the process runs the realization at least twice over: the tape worker walks
a live clone forward for the wire, and every 100 ms the sweeper restores a checkpoint
and re-walks the same span for triggers, plus another 300 s for volatility, plus
another residual for the last print. `/trades` restores again. Submits restore again.

The right architecture, and the hunter would take the rewrite: make the tape worker
the single forward walk and have it publish, alongside the broadcast frames, (a) a
running last-print cell, (b) an incrementally maintained `VOL_WINDOW_NS` estimator,
and (c) a bounded ring of recent prints covering at least one sweep interval plus a
slack factor. Then:

- `read_last` becomes an atomic load. `read_market` becomes an O(1) read of a
  maintained estimator. `scan_triggers` reads the ring for the ordinary case
  (`from_ns` inside the ring, which is the case for every resting order the sweeper
  has already seen once) and falls back to the checkpoint chain only for a cold order.
- The `MarketReadingCache` bucketing hack disappears entirely, and with it the
  documented behaviour cost in `fills.rs` - that submits are decided against "a
  reading NO OBSERVER CAN NAME". You get the exact per-submit reading back for free.
- The `spawn_blocking` fleet, the global `INDEX` mutex contention, and the 12.6 ms
  submit tax all go away.
- `CHECKPOINT_K` can then be sized purely for cold `/trades` paging, where a small K
  is strictly better.

The checkpoint chain stays for what it is actually good at (arbitrary historical
seeks); it stops being on the hot fill path.

## 5. `index()` validates the symbol only on the first call - wrong-symbol tape

`source.rs`:

```rust
if let Some(existing) = INDEX.get() { return Some(existing); }
let profile = profiles.get(symbol)?;
```

After the chain is initialized (i.e. always, post-warmup), any symbol string returns
the venue's one index. So `build_history_source("NOT-A-SYMBOL", ...)` returns a source
over BTCUSDT's realization, and `last_trade_at_or_before("NOT-A-SYMBOL", ts, ...)`
returns BTCUSDT's price. `/trades?symbol=GARBAGE` therefore returns a 200 with
BTCUSDT-labelled ticks instead of an empty page or a 400. `read_market` accidentally
survives this only because it does a second `profiles.get(symbol)?` for the
increment. `fills::scan_triggers` does not - it will happily walk the wrong tape for a
scan whose symbol is not the venue's. One venue per run makes this mostly unreachable
today, but it is a soundness hole dressed as an early return, and the fix is one line.

## 6. FlowSurge: the live wire permanently forks from the tape everything else reads

Confirmed from the server side: `Tape::arm_flow_surge` posts onto an mpsc that only
the tape worker drains, and the worker applies it to its own `source` clone. The
checkpoint chain, and therefore `/trades`, `/quotes`, `read_market`, `read_last` and
`scan_triggers`, never see it.

`docs/havoc.md` documents the half of this that is intentional: "Historical
checkpoint reads remain clean, so seeking back across a live surge intentionally does
not replay the havoc." What it does not document, and what the hunter believes is not
intended:

- The surge changes `dt_ns` and `children_mean` feeding `next_count`. Child count is
  an RNG draw whose value depends on the scaled mean. So the surge changes both the
  clock trajectory and the number of RNG draws consumed. The live clone's RNG state
  therefore diverges permanently. After the surge window expires, the WS stream never
  re-converges with `/trades` for the rest of the run. The doc's framing ("acts on the
  live tape ... for an absolute simulated-time window") reads as a bounded window; it
  is not.
- During the surge, a client watching prints that penetrate its resting limit does not
  get filled, because the sweeper is deciding against a different price path.
  `fills.rs` asserts the opposite invariant in its own words: "Composed from the same
  `build_history_source` the `/trades` cursor pages through, so the prints deciding a
  fill are the prints the client can fetch and check." That holds against `/trades`; it
  does not hold against the feed the client is actually subscribed to.

Either the surge should be armed on the shared index (making it a real regime change
everything sees), or `docs/havoc.md` needs to say plainly that arming a FlowSurge
permanently decouples the live feed from the fill-deciding tape. The hunter would take
the former.

## 7. FlowSurge arming is unobservably lossy

`tape.rs` twice does `drop(self.control.send(...))`. `http.rs` returns `202 Accepted`
unconditionally. If the tape worker exited - which it does silently when
`build_live_source` returns `None`: a `return` with no log, no error, no health
signal - every arm and every `ClearDivergences` is silently discarded while the
operator is told it was accepted.

Second delivery gap: the control channel is drained only between ticks. The worker
parks in `sleep_until_wall_cancellable` for a full inter-tick gap, which across a CME
closure is days of sim time (the calendar jump moves `clock_ns` to the next open). A
surge armed during a closure is not read until the reopen, by which point its absolute
`start_ns..end_ns` window is entirely in the past and it does nothing - with a 202
already returned. `TAPE_SLEEP_POLL` is 20 ms and the loop rechecks `cancel` but not
`controls`; moving the `try_recv` into the sleep loop is the cheap fix.

## 8. `last_trade_at_or_before` has no drain budget at all

`source.rs`. Every other tape consumer passes `SWEEP_DRAIN_BUDGET`: `scan_triggers`,
`read_market`. This one walks `while let Some(tick) = source.next_tick()` with the
only exit being a trade whose `ts_event > ts`. It terminates on today's generator (a
parent burst always ends in trades), but it is the one path with no ceiling, it is
called from the async sweeper (finding 1), and it is documented in `fills.rs` as the
fallback that runs precisely when `read_market` refused because its walk was truncated
- i.e. exactly the situation where an unbudgeted walk is longest.

## 9. Sweep-batch byte reservation may under-count venue-originated orders (plausible, medium confidence)

`sweeper.rs` reserves `swept_fill_max_bytes(shape, emitted + originated)`. `sizing.rs`
sizes that at four order-shaped frames per order. But `originated` counts liquidation
orders pushed through `Engine::on_submit`, and `worst_case_output_bytes`'s own
`SubmitOrder` arm reasons about five order-shaped frames (accepted, trigger,
duplicated fill, fill, remainder cancel). A margin-breach liquidation inside a sweep
pass is a submit, not a sweep fill, so it can emit the `OrderAccepted` that the swept
bound has no slot for. The per-order shape widening (+2 balances, +1 position, +1
margin) gives slack that probably absorbs it in practice, which is why this is flagged
as plausible rather than confirmed - but the derivation in `swept_fill_max_bytes`'s doc
comment enumerates sweep-fill frames only and does not contemplate an originated
submit, so the argument for domination is not made anywhere.

Also `deliver`'s `subject` attributes a whole-batch admission refusal to whichever
order happens to be first in the batch. A client that hit its budget gets
`AdmissionRejected` naming an unrelated order.

## 10. Smaller items

- `fill_golden.rs`: `(ts - ORIGIN).is_multiple_of(ACCEPT_STRIDE_NS)` is always true -
  `ACCEPT_STRIDE_NS == SWEEP_INTERVAL_NS`, and `ts` steps by exactly that. Dead
  condition. Also `censored: ORDERS_PER_OFFSET - filled` underflows and panics if an
  order is ever recorded twice (it cannot today - no divergence is armed - but the
  harness would panic rather than report if that changed).
- `gen.rs`: the reproduction test builds its "direct" source with `SizeGrid::spot()`
  while `build_source` uses `SizeGrid::from_def(&profile.def)`. They coincide for
  BTCUSDT, so the test passes for the wrong reason and would not catch a size-grid
  divergence in the CLI path.
- `tick_composition.rs`: `if recorded == parents && parent.parent_ts_ns >=
  fanout_end_ns { break }` discards the summary of the parent it just advanced.
  Harmless but it means the loop always advances one parent past its stopping
  condition. More notably, for `Mode::Surged` with `duration_ms = u64::MAX`,
  `start_ns.saturating_add(u64::MAX * 1_000_000)` saturates to `u64::MAX`, which is the
  intent, but it means the "surged" measurement is a permanently-surged tape, not a
  surge window - worth stating in the fixture's own metadata since the budget constants
  are derived from it.
- `tick_composition.rs`: `parent.child_stride_ns - 1` and division by
  `child_stride_ns`. Safe today only because `ParentSummary::child_stride_ns` is
  hardcoded to the `INTRA_EVENT_STEP_NS` constant. If that ever becomes a draw that can
  be zero, this is a divide-by-zero panic and an infinite loop, hours into a
  measurement run. A `NonZeroU64` on `ParentSummary` would make it unrepresentable.
- `tape.rs`: silent thread exit on an unresolvable symbol. No log, no readiness
  failure - the venue boots healthy and serves an eternally empty market data feed.
- `fills.rs`: `read_market(symbol, bucket_ns, ...)` - the sweeper's futures mark is
  therefore the price at the start of the bucket, up to `interval_ms` stale.
  `MarketReadingCache`'s doc reasons carefully about why staleness is acceptable for
  the band (a coarse scale) but the same entry is being used as a mark price for
  unrealized P&L, which is not a coarse scale. Under `speed < 1` several passes share
  one bucket and the mark freezes for all of them.
