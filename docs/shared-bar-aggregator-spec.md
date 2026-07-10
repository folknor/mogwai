# Shared time-bar aggregation core - implementation spec

## Standing references

- Contract this spec is written against:
  `reference/technical-implementation-spec.md`.
- Originating item: there is no spawned-from TODO document - this was designed in
  conversation as the enabling refactor for a future `mogwai gen` CLI (a
  nautilus-free command in `mogwai-server` that dumps the synthetic generator's
  output - trades, or aggregated OHLCV bars - offline, for visualizing the tape).
  The motivation is `docs/bug-sweep.md` under D16 (the multi-hour trade-desert
  question the CLI exists to make visible). The `mogwai gen` command is a
  separate, not-yet-written follow-on; it is named here only as a future
  CONSUMER, never cited as a dependency of this spec. Because that follow-on does
  not exist yet, this spec defines the shared core's full caller contract itself
  (see "Caller contract"), so it stands alone.

## Goal

Lift the trade-to-OHLCV time-bar aggregation math out of `mogwai-adapter` into a
nautilus-free module in `mogwai-data`, so a second consumer (the nautilus-free
`mogwai gen` CLI in `mogwai-server`) folds bars through the SAME implementation
instead of a hand-rolled copy. This is a pure refactor: the adapter's emitted
bar values stay byte-identical. The win is structural - one aggregation truth
shared by the adapter and the CLI, which also removes the drift risk between the
two right after the AD19/AD21 work touched this code.

## Stopping rule / scope

IN scope:
- A new nautilus-free `mogwai-data` bar-aggregation module holding the
  window-boundary math and the OHLCV fold, plus its unit tests, with a
  fully-defined caller contract (interval nonzero, timestamps nondecreasing).
- Re-pointing the adapter's aggregation cluster at that module, deleting the
  adapter's now-duplicated types/helpers, and adding the `mogwai-data`
  dependency the re-point needs to compile.
- Adding a per-window `count` field to the shared accumulator (the CLI needs
  `trade_count`; adding it now, inert on the adapter side, avoids a second edit
  to the core in the CLI spec).
- Strengthening the adapter/core test coverage enough that the byte-identity
  claim is actually gated at nontrivial precision (see "Verification").

OUT of scope (named, not deferred - each belongs to a genuinely separate item):
- The `mogwai gen` command itself.
- Empty-window filling (emit a row per interval including zero-trade windows).
  That is a CLI presentation concern; the shared core emits a bar only for a
  window that had trades, exactly as the adapter does today.
- Interval / duration string parsing (`5m` -> ns). CLI-only. NOTE: rejecting a
  zero interval at parse time is the CLI's job; the shared core is nonetheless
  made total against a zero interval by TYPE (see "Caller contract"), so no
  caller can trip it.
- Any visualization / charting tool.
- Any change to the adapter's lazy-emit contract, the AD19 teardown/unsubscribe
  flush semantics, the calendar-anchoring refusal (`is_calendar_anchored`), or
  the nautilus `Bar` conversion beyond a mechanical rename and preserving its
  existing warn-and-drop-on-error handling. Those stay in the adapter.

## Survey of the ground

The only trade-to-OHLCV folder in the tree lives in
`crates/mogwai-adapter/src/client/data.rs`. Inventory of the cluster (by name -
line numbers drift):

- Types: `ActiveBar` (`open`, `high`, `low`, `close` as `Decimal`, `volume` as
  `Decimal`, `close_ts` as `u64`; derives `Debug` only) and `BarSubState`
  (`refs: usize`, `active: Option<ActiveBar>`; derives `Debug, Default`).
- `update_bar_state`: derives the interval from the nautilus `BarType` via
  `get_bar_interval_ns(&bar_type).as_u64()`, computes the window close as
  `((ts / interval) + 1) * interval`, folds `high.max` / `low.min` / `close =` /
  `volume +=`, and rotates on `ts >= active.close_ts` by building the closed
  window's bar (through `active_to_bar`) and starting a fresh window via
  `new_active_bar`. On a rotate where `active_to_bar` returns `Err`, it logs
  `"dropping unrepresentable bar"` and returns `None` but STILL rotates the
  window (one bad bar must not wedge aggregation).
- `aggregate_bars`: the batch/history path - loops trades through
  `update_bar_state`, then flushes the trailing window only when the request's
  `end >= active.close_ts`, with the same warn-and-drop-on-`Err` on that flush.
- `new_active_bar`: `ActiveBar` from one trade (all four prices = trade price,
  volume = trade size).
- `active_to_bar`: the ONLY nautilus-bound step besides the interval derivation -
  builds `Bar::new(...)` via `convert::price` / `convert::quantity` at the
  instrument's declared precision, stamping `ts_event = close_ts`.

Callers of the cluster (all in the same file), which the rip must keep working:
- `emit_live_bars` -> `update_bar_state` (the live WS/poll bar path).
- the `request_bars` spawn -> `aggregate_bars` (the history path).
- `unsubscribe_bars` -> `active_to_bar` (the AD19 unsubscribe completed-window
  flush).
- `flush_completed_bars` -> `active_to_bar` (the AD19 `stop()` teardown flush).
- Tests constructing `ActiveBar` / `BarSubState` directly and calling
  `update_bar_state` / `aggregate_bars`:
  `request_bar_aggregation_closes_on_window_and_drops_partial`,
  `request_bar_aggregation_flushes_trailing_completed_window`,
  `live_bar_state_emits_only_on_window_close`,
  `unsubscribe_bars_flushes_completed_window_but_not_in_progress`,
  `stop_flushes_completed_bar_windows_but_not_in_progress`.

Findings from the survey:
- The fold and window math are already pure `Decimal`/`u64`. Nautilus enters the
  cluster at exactly two points - the interval derivation
  (`get_bar_interval_ns(&BarType)`) and `active_to_bar` (the `Bar` /
  `InstrumentId` / precision conversion). Both are the ADAPTER's job and stay in
  the adapter; the interval reaches the shared core as a plain nonzero integer
  and the shared core hands back a plain accumulator the adapter converts.
- No consumer of `ActiveBar` / `BarSubState` / `new_active_bar` /
  `active_to_bar` exists outside `data.rs` (crate-wide grep); `ActiveBar` derives
  only `Debug`, so no serde/derive coupling breaks when it is replaced.
- Host crate for the core: `mogwai-data` already depends on `rust_decimal` and
  `mogwai_protocol` and has a private `mod generated;` re-exported at the crate
  root via `pub use generated::{...}`. The new module follows that SAME private-
  module-plus-root-re-export convention (see "Target").
- COMPILE-BLOCKING dependency gap: `crates/mogwai-adapter/Cargo.toml` currently
  declares only `mogwai-protocol.workspace = true` (plus nautilus crates) - it
  does NOT depend on `mogwai-data`. The re-point's `use mogwai_data::{...}` will
  not compile until that dependency is added. The workspace dependency table
  (root `Cargo.toml`) already carries `mogwai-data = { path = ... }`, so this is
  a one-line addition (`mogwai-data.workspace = true`), but it is a required
  brick, not an assumed one. `mogwai-server` already depends on `mogwai-data`,
  so the future CLI reaches the core with no manifest change.

## Caller contract

The shared core is a public API with two consumers (the adapter now, the CLI
later), so its preconditions are stated and enforced here rather than left to a
trusted call site:

- Interval is NONZERO, enforced by TYPE: the interval parameter is
  `std::num::NonZeroU64`, so a zero interval cannot reach the division and the
  `interval_ns == 0` panic is unrepresentable. The adapter constructs it from
  `get_bar_interval_ns` (always positive for the Day-and-finer aggregations it
  admits - tick/volume and calendar aggregations are already refused upstream) via
  `NonZeroU64::new(interval).expect(...)` on that invariant. The CLI rejects a
  zero interval at flag-parse time before constructing the `NonZeroU64`.
- The window-close arithmetic is TOTAL: `((ts / iv) + 1) * iv` is computed with
  saturating add and multiply, so a pathological interval near `u64::MAX` yields
  a `close_ts` of `u64::MAX` (a window that never closes - the tail stays open,
  a defined outcome) instead of wrapping. No input panics or overflows.
- Timestamps are NONDECREASING across a fold sequence. `fold_trade` assumes each
  trade's `ts` is `>=` the previous: an out-of-order trade with `ts` below the
  active window's `close_ts` is folded into the active (later) window, and one at
  or above it wrongly rotates. Both real consumers satisfy this - the adapter's
  live path drains an ascending `MergeSource` and its poll path an ascending
  cursor; the CLI's `GeneratedSource` emits monotone `ts_event`. The precondition
  is documented on `fold_trade`; it is a contract, not a runtime check.

## Target - concrete artifacts

New file `crates/mogwai-data/src/bars.rs`, declared `mod bars;` in
`crates/mogwai-data/src/lib.rs` with `pub use bars::{BarAcc, window_close_ns,
fold_trade};` at the crate root - matching the existing private-`mod generated;`
plus `pub use generated::{...}` convention exactly, so consumers name the items
as `mogwai_data::{BarAcc, fold_trade, window_close_ns}`. Nautilus-free; depends
only on `rust_decimal::Decimal` and `core::num::NonZeroU64`.

```rust
use core::num::NonZeroU64;

use rust_decimal::Decimal;

/// In-progress OHLCV accumulator for one time-bar window. Pure `Decimal`; the
/// window it belongs to closes at `close_ts` (exclusive upper bound,
/// epoch-anchored). `count` is the number of trades folded into the window so
/// far. Fields are public: both consumers read them (the adapter to convert to
/// a nautilus `Bar`, the CLI to format a CSV row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarAcc {
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub count: u64,
    pub close_ts: u64,
}

/// The epoch-anchored close of the window a trade at `ts` falls in, for a bar
/// `interval`: `((ts / interval) + 1) * interval`. This is the exact anchoring
/// the adapter used (matching nautilus `get_bar_interval_ns` for Day-and-finer
/// aggregations; calendar anchoring for Week/Month/Year is out of scope and
/// stays refused adapter-side). Total by construction: `interval` is nonzero by
/// type, and the arithmetic saturates, so a huge interval yields `u64::MAX`
/// (a never-closing window) rather than wrapping.
#[must_use]
pub fn window_close_ns(ts: u64, interval: NonZeroU64) -> u64 {
    let iv = interval.get();
    (ts / iv).saturating_add(1).saturating_mul(iv)
}

/// Fold one trade into the running window `state`. Returns the just-CLOSED
/// window's accumulator when the trade rotates into a new window; `None` when it
/// extends the current window or opens the first one. This reproduces the
/// adapter's `update_bar_state` rotate semantics exactly: open on the first
/// trade (all prices = trade price, volume = size, count = 1), fold with
/// `high.max` / `low.min` / `close =` / `volume +=` / `count += 1`, and rotate
/// on `ts >= active.close_ts` returning the old window unchanged.
///
/// PRECONDITION: `ts` is nondecreasing across calls for a given `state` (see the
/// spec's Caller contract). An out-of-order `ts` folds into or rotates the wrong
/// window; it is a contract violation, not a checked error.
///
/// `#[must_use]`: ignoring the return silently drops a closed bar.
#[must_use]
pub fn fold_trade(
    state: &mut Option<BarAcc>,
    price: Decimal,
    size: Decimal,
    ts: u64,
    interval: NonZeroU64,
) -> Option<BarAcc> {
    let close_ts = window_close_ns(ts, interval);
    match state {
        Some(active) if ts >= active.close_ts => {
            let closed = active.clone();
            *state = Some(BarAcc::open(price, size, close_ts));
            Some(closed)
        }
        Some(active) => {
            active.high = active.high.max(price);
            active.low = active.low.min(price);
            active.close = price;
            active.volume += size;
            active.count += 1;
            None
        }
        None => {
            *state = Some(BarAcc::open(price, size, close_ts));
            None
        }
    }
}

impl BarAcc {
    fn open(price: Decimal, size: Decimal, close_ts: u64) -> Self {
        Self {
            open: price,
            high: price,
            low: price,
            close: price,
            volume: size,
            count: 1,
            close_ts,
        }
    }
}
```

Data flow after the change:

- Adapter live path: `update_bar_state` derives `interval_ns` from the
  `BarType`, wraps it as `NonZeroU64::new(interval_ns).expect("admitted bar
  aggregations have a positive interval")`, calls `bars::fold_trade(&mut
  state.active, trade.price, trade.size, trade.ts_event, interval)`, and on a
  returned `BarAcc` converts it to a nautilus `Bar` via the renamed
  `acc_to_bar` - PRESERVING the existing warn-and-drop-on-`Err` (`"dropping
  unrepresentable bar"`, return `None`). The window has already rotated inside
  `fold_trade`, so the "one bad bar doesn't wedge aggregation" property is now
  structural (the rotation no longer depends on the conversion succeeding). The
  `?`-early-return on a `None` from `fold_trade` keeps the "emit only on window
  close" contract.
- Adapter history path: `aggregate_bars` loops `fold_trade`, and its trailing
  flush reads `acc.close_ts` (was `active.close_ts`) and calls `acc_to_bar` with
  the SAME warn-and-drop-on-`Err` (`"dropping unrepresentable trailing bar"`),
  unchanged in logic.
- Adapter teardown paths: `unsubscribe_bars` and `flush_completed_bars` read
  `acc.close_ts` and call `acc_to_bar`, unchanged in logic.
- CLI (future spec): calls `bars::fold_trade` over the generator's trade stream
  and formats each returned `BarAcc` as a CSV row; empty-window filling is done
  in the CLI layer, not here.

Adapter-side edits (all in `crates/mogwai-adapter/`):
- `Cargo.toml`: add `mogwai-data.workspace = true` under `[dependencies]`.
  (`Cargo.lock` will change; commit it, per the standing git rules.)
- `src/client/data.rs`: delete `struct ActiveBar` and `fn new_active_bar`;
  `use mogwai_data::{BarAcc, fold_trade};` (and `std::num::NonZeroU64`).
- `BarSubState.active: Option<BarAcc>`.
- `update_bar_state`: replace the inline fold/rotate with the `NonZeroU64`
  construction plus a `fold_trade` call as above, keeping the warn-and-drop
  conversion.
- Rename `active_to_bar` -> `acc_to_bar`, parameter `active: &ActiveBar` ->
  `acc: &BarAcc`; body unchanged (it already reads only the pure fields, and
  ignores `count`, which nautilus `Bar` has no slot for). Because the body is
  byte-for-byte the old `active_to_bar`, the conversion cannot drift.
- Tests: the two `active_bar(close_ts)` helper fns (in the unsubscribe and stop
  flush tests) return `BarAcc { ..., count: 1 }`, and the direct `ActiveBar {
  .. }` literals in the live/aggregate tests become `BarAcc { ..., count: N }`.
  `BarSubState { .. }` literals stay `BarSubState` - only their inner `active`
  value changes type. (This is the precise edit: it is NOT "every `BarSubState`
  literal becomes `BarAcc`".)

## Landing

One coherent, fully intrusive landing (the extraction cannot be half-wired and
stay green): add the module, add the adapter dependency, re-point the adapter,
strengthen the tests, in a single commit.

Ordered bricks within the landing:

1. Add `crates/mogwai-data/src/bars.rs` (`BarAcc`, `window_close_ns`,
   `fold_trade`, `BarAcc::open`) and the `mod bars;` + `pub use` in `lib.rs`. Add
   `#[cfg(test)] mod tests` pinning, at EXACT `Decimal` values (see Verification
   for why exactness matters):
   - `window_close_ns` anchoring: a ts mid-window and a ts exactly on a boundary;
     interval `1`; a huge interval near `u64::MAX` and a large `ts`, asserting
     the saturating result (`u64::MAX`, no panic/wrap).
   - open-on-first: all prices equal, volume = size, count = 1, correct
     `close_ts`.
   - a non-rotating trade returns `None` and updates high/low/close/volume/count.
   - a rotating trade returns the OLD window unchanged and starts the new one.
   - a multi-trade sequence at NON-round prices/sizes (e.g. `100.07`, `99.93`,
     size `0.333`) asserting the exact `Decimal` O/H/L/C/V/count/close_ts, so a
     precision or field-assignment bug in the fold is caught (round values would
     hide it).
2. Add `mogwai-data.workspace = true` to `crates/mogwai-adapter/Cargo.toml`.
3. Re-point `crates/mogwai-adapter/src/client/data.rs` per the edit list above,
   including the `NonZeroU64` construction, the preserved warn-and-drop, and all
   test-literal updates.
4. Add an adapter regression that asserts ALL converted OHLCV fields
   (`open`/`high`/`low`/`close`/`volume`) of an emitted `Bar` at a NONTRIVIAL
   precision - trade prices whose fold result exercises sub-unit decimals at the
   instrument's declared `price_precision`/`size_precision`, so a divergence in
   `fold_trade` shows up in the converted values (the existing aggregation tests
   use round values that are f64-exact and would not).

## Verification per brick

The load this refactor must prove is: the adapter's emitted bar VALUES are
byte-identical (pure refactor), and the new core behaves as specified. Correcting
the earlier draft's overclaim: the PRE-EXISTING adapter bar tests are necessary
but NOT sufficient as a byte-identity proof - their aggregation assertions
compare `bar.<field>.as_f64()` against ROUND values (100.0, 200.0, whole-number
volumes) that are f64-exact, and the two AD19 teardown tests assert only
`ts_event`, never OHLCV. A fold that mangled sub-unit precision or misassigned a
field could pass them. The byte-identity gate is therefore the NEW exact-`Decimal`
core tests (brick 1) plus the NEW nontrivial-precision adapter regression
(brick 4); the pre-existing tests still gate the structural behavior (window
close, lazy emit, trailing/teardown flush).

- Brick 1 gate (the core behaves, at exact precision):
  `brokkr test -p mogwai-data bars`
  (No existing `mogwai-data` test name contains `bars`, so the substring matches
  only the new `bars::tests::*`.)
- Bricks 2-3 gate (the adapter still compiles and its structural bar behavior is
  unchanged): the adapter bar tests. Exact command:
  `brokkr test -p mogwai-adapter bar`
  NOTE the substring `bar` matches NINE tests in `data.rs`, not five: the five
  in the survey PLUS `request_bars_off_tape_window_errors_loudly`,
  `bar_span_stops_once_enough_intervals_are_covered`,
  `unmatched_unsubscribe_bars_does_not_darken_surviving_feed`, and
  `calendar_anchored_bar_aggregations_are_refused`. All nine are bar-related and
  unaffected by the refactor, so the wider match is fine - it is not an
  exhaustive list of the aggregation tests, just the filter's actual reach.
- Brick 4 gate (byte-identity at precision): the new regression is inside that
  same `brokkr test -p mogwai-adapter bar` run.
- Whole-landing gate (gremlins, clippy, every crate's tests, changed-files
  scope), the single command the commit is judged on:
  `brokkr check`

## Keep / revert

Kept only if: `brokkr check` is green, the new exact-`Decimal` core tests and the
new nontrivial-precision adapter regression pass, AND the pre-existing adapter
bar tests pass with NO edit to their assertions (only the mechanical `ActiveBar`
-> `BarAcc { ..., count }` literal change). If reproducing the fold in
`fold_trade` changes any adapter bar assertion value, or the new precision
regression disagrees with the pre-refactor output, the extraction diverged -
revert and re-align `fold_trade` to `update_bar_state`'s exact arithmetic before
landing. There is no partial-keep: the adapter either aggregates through the
shared core with identical output or the landing is reverted whole.
