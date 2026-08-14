# Bug hunt: mogwai-server tape and fill path

Scope: `source.rs`, `tape.rs`, `gen.rs`, `fills.rs`, `fill_golden.rs`,
`tick_composition.rs`, `sweeper.rs`.

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Cross-scope, for whoever picks up `bugs-data.md`: finding 6 here confirms and
extends its finding 1. Its finding 2 (`BoundedSeek` cited by docs that do not
have it) is CLOSED by round 1 here - the identifier is gone from the tree and
`try_source_at_or_before` is the real refusal. Its finding 4 quotes
`CHECKPOINT_K` as 4,194,304; round 1 set it to 8,192, so that finding's
arithmetic is stale even where its shape survives, and what survives of it is
restated as 4a below.

Findings 1 through 5 were closed in round 1. Finding 4 was closed only in part,
and what it left open is restored below as finding 4a rather than carried in a
commit message nobody will read again.

## 4a. Every skipped tick of a checkpoint walk is fully materialized

The RESIDUAL of finding 4, separated out because it is independent of the
FlowSurge ruling that fenced the rest. Finding 4 proposed one live forward walk
publishing a print ring and an incremental volatility estimator; that rewrite
decides how the live wire and the fill-deciding tape relate, which is finding 6,
so it stays deferred. This part decides nothing of the sort.

`GeneratedSource` does not override `TickSource::seek_to`, so the trait default
loops `next_tick`. Both places that skip ticks therefore pay full materialization
for every one of them:

- `CheckpointIndex::extend_toward` walks the lead with `next_tick`. The boot
  warmup is exactly this walk - 4,288,935 ticks on the committed default config,
  measured at 2.9M ticks/s.
- the residual replay after a restore, through `MergeSource::starting_at`'s
  default `seek_to`.

Each skipped tick allocates a `Symbol` String and constructs two `Decimal`s that
are immediately discarded. `GeneratedSource::advance_parent` already advances
without materializing, and `tick_composition.rs` demonstrates it agrees with the
wire walk, so a `seek_to` override that advances whole parents and materializes
only the tail parent is the shape of the fix.

Round 1 shrank `CHECKPOINT_K` from 67,108,864 to 8,192, which cut the number of
ticks a restore replays by 53x. It did not make a skipped tick cheaper, and it
does not touch the warmup walk at all, which is where the multiple is largest.

Cost check before doing it: this changes no emitted tick, but it does change the
generator's execution path, and anything that moves a draw ordering owes a
`TAPE_PROTOCOL_VERSION` bump. A `seek_to` that reuses `advance_parent` must be
proven to consume the identical draws, by golden, before it lands.

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
