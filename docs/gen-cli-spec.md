# `mogwai gen` CLI - implementation spec

## Standing references

- Contract this spec is written against:
  `reference/technical-implementation-spec.md`.
- Prerequisite it builds on: `docs/shared-bar-aggregator-spec.md` (Spec 1, now
  landed) - the nautilus-free `mogwai_data::{BarAcc, fold_trade, window_close_ns}`
  aggregation core this CLI folds bars through instead of a hand-rolled copy.
- Originating item: designed in conversation as the D16 desert-visualization
  instrument. Motivation is `docs/bug-sweep.md` under D16 (a tape that can go
  near-silent for hours; this tool makes that visible). There is no spawned-from
  TODO document; the full UX is settled in this spec.

## Goal

Add a `mogwai gen` subcommand that runs the synthetic generator OFFLINE (no
server, no sockets, no adapter) and writes its output as CSV - either raw trades
or aggregated OHLCV bars - so the generated tape can be charted and inspected,
starting with the D16 deserts. Bars are folded through the shared Spec-1 core.

What "reproduce the tape" means precisely: the walk is a deterministic function
of `(symbol-derived scalars/session, seed, anchor)`. This CLI uses the SAME
fingerprint profile and the SAME FNV seed the running server keys a symbol on, so
for a given `--start` anchor it produces the identical walk the server would
produce at that same anchor. The default `--start 0` is a canonical, reproducible
tape that exhibits the same desert PHENOMENON (the D16 mechanism is
anchor-independent per `docs/bug-sweep.md`), NOT the exact bytes a particular
server boot serves - the server anchors every generator at its boot-derived
`data_origin`, which is neither 0 nor stable across runs. To reproduce a specific
served run, pass `--start <that run's data_origin>`.

## Stopping rule / scope

IN scope:
- A `Gen(GenArgs)` subcommand on the existing clap `Command` enum, dispatched to
  a new `crate::gen` module.
- Trade output and bar output (fixed-interval OHLCV) with empty-window filling so
  interior and trailing deserts render as flat zero-volume runs.
- A duration grammar (`<n><unit>`) for `--length` and `--interval`.
- The `--type`, `--length`, `--interval`, `--symbol`, `--seed`, `--start`,
  `--start-price`, `--regime`, `--out` flags below.
- Reuse (not reinvent) the server's generation plumbing: `InstrumentProfiles`,
  `seed_for` and `fingerprint()` (both made `pub(crate)`), `GeneratedSource`, and
  the Spec-1 bar core.
- Streaming output (O(1) memory): rows are written as they are produced, so a
  large `--length`/`--interval` does not materialize a giant Vec.

OUT of scope (named, not deferred - each a separate future item):
- The `--havoc <file>` convenience (accept a full `HavocSpec` JSON and honor only
  its `.data` surface). `--regime` covers the applicable data surface for v1.
- Config-driven instruments (`--config mogwai.toml` with `[[instrument]]`
  tables). v1 serves the built-in fingerprint venue; `--symbol` selects among the
  default set (today just BTCUSDT).
- A human-readable / ISO time column. `mogwai-server` has no `chrono` dependency;
  output is lossless unix-ns, which every charting tool parses.
- The transport-corruption lens (client havoc drop/dup/reorder on the emitted
  stream) - a separate `--as-received` mode if ever wanted.
- Any visualization / charting tool that consumes this CSV.
- A bundled `man` topic for `gen` (clap `--help` from the doc comments suffices).

## Survey of the ground

CLI structure (`crates/mogwai-server/src/main.rs`): a clap `#[derive(Parser)]
struct Cli` with `#[command(subcommand)] command: Command`, and
`enum Command { Serve(ServeArgs), Stop(StopArgs), Man { topic } }`. `fn main`
matches on `Command` and dispatches. Subcommand arg structs derive
`#[derive(Args)]`; `man.rs` is the precedent for a per-command module. Adding
`Gen(GenArgs)` plus a match arm is the established extension point.

Generation plumbing (`crates/mogwai-server/src/source.rs`), reuse surface:
- `InstrumentProfiles::defaults()` and `.get(symbol) -> Option<&InstrumentProfile>`
  are `pub(crate)`. `InstrumentProfile { def, scalars, session }` fields are
  `pub(crate)`. `GeneratorScalars.start_price` is `pub`. All reachable from a
  sibling `crate::gen` module.
- `seed_for(symbol) -> u64` (the FNV the server keys each symbol's walk on) is
  PRIVATE. The CLI must use the SAME seed to reproduce the served walk, so it is
  made `pub(crate)` rather than re-deriving the FNV. Required brick.
- `fingerprint() -> &'static Fingerprint` is a private `OnceLock` accessor.
  `InstrumentProfiles::defaults()` already loads through it. The CLI also needs a
  `&Fingerprint` for the generator constructor, so `fingerprint()` is made
  `pub(crate)` and the CLI reuses it - one parse of the committed JSON per run,
  shared with `defaults()`, instead of a second `Fingerprint::from_repo_json()`.
  Required brick.

Generator API (`mogwai-data`, public and re-exported at the crate root, all
verified against the current tree):
`GeneratedSource::try_new_with_session_profile(scalars, seed, start_ts, &fp,
&session, regime) -> Result<GeneratedSource, GeneratedSourceError>` builds the
walk anchored at `start_ts`, validating `scalars`/`session` (used here because
`--start-price` overrides a scalar - a bad value is an error, not a panic).
`GeneratedSource` implements `TickSource`, whose `next_tick(&mut self) ->
Option<TickEvent>` yields `TickEvent::Trade(TradeTick)` (the generator emits only
trades; aggressor is set internally, never `Quote`). `TradeTick`
(`mogwai-protocol`) carries `symbol`, `price: Decimal`, `size: Decimal`,
`aggressor: AggressorSide`, `ts_event: u64`.

Shared bar core (Spec 1, `mogwai-data`, verified): `fold_trade(&mut
Option<BarAcc>, price, size, ts, interval: NonZeroU64) -> Option<BarAcc>` returns
a closed window on rotation (`ts >= active.close_ts`), `#[must_use]`; `BarAcc {
open, high, low, close, volume: Decimal, count: u64, close_ts: u64 }`. The
window grid is epoch-anchored (`window_close_ns`), INDEPENDENT of `--start`. The
CLI folds trades through this and does its OWN empty-window filling (a CLI
presentation concern Spec 1 left to the consumer). Nondecreasing-ts precondition
holds: `GeneratedSource` emits monotone `ts_event`.

Regime: `MarketRegime` is `#[serde(tag = "type")]` (INTERNALLY tagged - see
`havoc.rs`), so a `--regime` value is JSON like
`{"type":"LiquidityDrought","thin_factor":5.0}`. `validate_market_regime` is
public in `mogwai-protocol`. `LiquidityDrought { thin_factor }` divides arrival
intensity - the directly D16-relevant lens.

Dependencies: `mogwai-server` already depends on `clap` (derive), `serde_json`,
`rust_decimal`, `anyhow`, `mogwai-data`, `mogwai-protocol`, and has NO nautilus
dependency (this stays a nautilus-free crate). No manifest change is needed.

## Target - concrete artifacts

New module `crates/mogwai-server/src/gen.rs`, declared `mod gen;` in `main.rs`.
Two visibility edits in `source.rs` (`seed_for` and `fingerprint()` -> `pub(crate)`).
One `Command` variant and one dispatch arm in `main.rs`.

```rust
// crates/mogwai-server/src/gen.rs
use std::io::Write;
use std::num::NonZeroU64;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, ValueEnum};
use mogwai_data::{BarAcc, TickEvent, TickSource, fold_trade};
use mogwai_protocol::{AggressorSide, MarketRegime, TradeTick, validate_market_regime};
use rust_decimal::Decimal;

use crate::source::{InstrumentProfiles, fingerprint, seed_for};

#[derive(Copy, Clone, ValueEnum)]
pub(crate) enum GenType {
    Trades,
    Bars,
}

/// `mogwai gen` arguments. Dumps the offline generator as CSV.
#[derive(Args)]
pub(crate) struct GenArgs {
    /// What to emit: raw trades, or aggregated OHLCV bars.
    #[arg(long = "type", value_enum, default_value = "trades")]
    kind: GenType,
    /// Sim-time span to generate: `<n><unit>`, unit one of s m h d w mo y
    /// (mo = 30d month, y = 365d year). Row count follows from it.
    #[arg(long, default_value = "1d")]
    length: String,
    /// Bar interval, same grammar as `--length`. Required with `--type bars`,
    /// rejected with `--type trades`.
    #[arg(long)]
    interval: Option<String>,
    /// Instrument to generate. Selects a built-in fingerprint profile.
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    /// Walk seed. Defaults to the FNV of `--symbol`, matching the running server.
    #[arg(long)]
    seed: Option<u64>,
    /// Sim-time unix-ns anchor the walk starts at. Default 0 is the canonical
    /// from-origin tape (anchor-independent desert phenomenon; see the Goal).
    #[arg(long, default_value_t = 0)]
    start: u64,
    /// Override the profile's start price (the price-scale anchor).
    #[arg(long)]
    start_price: Option<Decimal>,
    /// Data-regime havoc as internally-tagged JSON, e.g.
    /// `{"type":"LiquidityDrought","thin_factor":5.0}`. Validated by
    /// `validate_market_regime`.
    #[arg(long)]
    regime: Option<String>,
    /// Write CSV here instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

pub(crate) fn run(args: GenArgs) -> anyhow::Result<()>;

/// `<n><unit>` -> nanoseconds. `<n>` is one-or-more ASCII digits (`>= 1`);
/// `<unit>` is EXACTLY one of `s m h d w mo y` (case-sensitive, no surrounding
/// or internal whitespace: `"1 d"`, `"1D"`, `"1"`, `"d"`, `"0d"` all error).
/// Multipliers: s=1e9, m=60e9, h=3600e9, d=86_400e9, w=604_800e9, mo=30d, y=365d.
/// Total; rejects empty/zero/unknown-unit and saturating-checks overflow to an
/// error rather than panicking.
fn parse_duration(s: &str) -> anyhow::Result<u64>;

/// Resolve profile + seed + start-price override + regime and build the walk via
/// the fallible constructor (a bad `--start-price` is an error, not a panic).
fn build_source(args: &GenArgs) -> anyhow::Result<mogwai_data::GeneratedSource>;

/// Stream trade rows for `ts_event` in `[start, end)`:
/// header `ts_event,price,size,aggressor`, then one row per trade. Takes an
/// iterator of trades (not a concrete source) so tests feed a crafted sequence.
fn write_trades(
    trades: impl Iterator<Item = TradeTick>,
    out: &mut impl Write,
) -> anyhow::Result<()>;

/// Stream bar rows over the epoch-anchored grid, one row per window FULLY inside
/// `[start, end)`, INCLUDING zero-trade windows (see the empty-fill rule).
/// header `open_ts,close_ts,open,high,low,close,volume,trade_count`. Takes an
/// iterator of trades so tests can craft deserts at chosen positions (the real
/// generator's deserts emerge hours in, seed-dependently, and cannot be dialed).
fn write_bars(
    trades: impl Iterator<Item = TradeTick>,
    start: u64,
    end: u64,
    interval: NonZeroU64,
    out: &mut impl Write,
) -> anyhow::Result<()>;
```

Control flow of `run`:
1. Parse `--length` to `len_ns`; `end = start.saturating_add(len_ns)`.
2. Validate the `--type`/`--interval` pairing: `bars` REQUIRES `--interval`
   (parse it, reject 0 via `NonZeroU64::new(..).context(..)`); `trades` REJECTS a
   present `--interval` (`bail!`).
3. `build_source(&args)?`.
4. Adapt the source to a bounded trade iterator:
   `let trades = std::iter::from_fn(move || match source.next_tick() {
   Some(TickEvent::Trade(t)) => Some(t), _ => None })
   .take_while(|t| t.ts_event < end);`
   (The generator is unbounded and trade-only; `take_while` bounds the span and
   the bar end-flush covers the last in-span window whose rotating trade is >=
   `end`.)
5. Open the sink: `--out` file or `std::io::stdout()`, in a `BufWriter`.
6. `GenType::Trades => write_trades(trades.filter(|t| t.ts_event >= args.start), &mut out)`;
   `GenType::Bars => write_bars(trades, args.start, end, interval, &mut out)`.
   Flush.

`build_source`:
- `let fp = fingerprint();`
- `let profiles = InstrumentProfiles::defaults();`
- `let profile = profiles.get(&args.symbol).with_context(|| format!("unknown
  symbol {}: the built-in venue does not list it", args.symbol))?;`
- `let seed = args.seed.unwrap_or_else(|| seed_for(&args.symbol));`
- `let mut scalars = profile.scalars.clone(); if let Some(p) = args.start_price {
  scalars.start_price = p; }`
- `let regime = args.regime.as_deref().map(parse_regime).transpose()?;` where
  `parse_regime` does `serde_json::from_str::<MarketRegime>` then
  `validate_market_regime` (mapping the `&'static str` error into `anyhow`).
- `GeneratedSource::try_new_with_session_profile(scalars, seed, args.start, fp,
  &profile.session, regime).context("building the generator")`.

Empty-fill rule (the crux; streamed; corrected for the straddle and start-edge
cases). A window with close `C` and open `C - iv` is emitted iff it is FULLY
inside `[start, end)`: `C - iv >= start` AND `C <= end`. State: `prev_close:
Option<u64>` (last emitted window's close) and `carry: Option<Decimal>` (last
emitted close price; `None` until the first real bar - this makes the
leading-skip STRUCTURAL). A helper `fill_empty_to(target, out)` emits, for each
grid boundary `nb` with `prev_close = Some(pc)`, `nb = pc + iv`, `nb <= target`
AND `nb <= end`, and `carry = Some(price)`: an empty window `[nb - iv, nb)` with
`open=high=low=close=price`, `volume=0`, `count=0`; advancing `prev_close`.

- Fold each trade (`ts < end`, all `>= start`) through `fold_trade`. On a returned
  closed `BarAcc` with close `C`: `fill_empty_to(C, out)`; then emit the real bar
  ONLY IF `C - iv >= start` (a start-straddling first window is skipped, exactly
  like a leading empty - `carry` stays `None`, so no empties precede it either);
  on emit set `prev_close = Some(C)`, `carry = Some(bar.close)`.
- After the fold loop, END-FLUSH the still-open accumulator `acc` (if any): if
  `acc.close_ts <= end` (the span proves it complete) AND `acc.close_ts - iv >=
  start`, `fill_empty_to(acc.close_ts, out)` then emit `acc` and update state; if
  `acc.close_ts > end` it straddles `end` and is DROPPED (not emitted).
- TRAILING EMPTIES, UNCONDITIONALLY (the fix): after the end-flush attempt, call
  `fill_empty_to(end, out)`. This fills a desert between the last emitted bar and
  the span end REGARDLESS of whether the open accumulator flushed or dropped - so
  a desert near the end followed by a late partial burst (whose window straddles
  `end` and drops) still renders its empty windows.
- Consequences: a LEADING desert renders as ABSENT rows (no price to carry, no
  trade to price them); INTERIOR and TRAILING deserts render as flat zero-volume
  runs. An `--interval` longer than `--length` (no window fully inside the span)
  yields header-only output. Row count is bounded only by `--length / --interval`
  (user's choice) and rows stream, so memory stays O(1).

CSV formatting: `Decimal` via `Display` (exact, lossless); `ts`/`open_ts`/
`close_ts` as `u64`; empty-window `volume` as `Decimal::ZERO`; `aggressor` as a
lowercase word via a small `match` (`buyer`/`seller`/`none`). `main.rs`: add
`Gen(GenArgs)` with a `/// Dump the offline generator as CSV (trades or bars).`
doc comment and `Command::Gen(args) => gen::run(args)`.

## Landing

One coherent additive landing (new module + one `Command` variant + two
visibility edits): it compiles and the suite stays green in a single commit; no
intermediate broken state.

Ordered bricks:
1. `source.rs`: `seed_for` and `fingerprint` -> `pub(crate)`.
2. `crates/mogwai-server/src/gen.rs`: `parse_duration`, `parse_regime`, `GenType`,
   `GenArgs`, `build_source`, `write_trades`, `write_bars`, `run`, plus
   `#[cfg(test)] mod tests` (see Verification).
3. `main.rs`: `mod gen;`, the `Gen(GenArgs)` variant, the dispatch arm.

## Verification per brick

The CLI is new behavior no existing test reaches, so the instruments are built
here, each a brick. `write_trades`/`write_bars` take a trade ITERATOR, so every
functional test drives them with a crafted `Vec<TradeTick>` into an in-memory
`Vec<u8>` sink - no process spawning, and deserts are placed exactly. A tiny
`fn tt(ts: u64, price: &str, size: &str) -> TradeTick` helper builds the crafted
trades.

- `parse_duration`: each unit (`s m h d w mo y`) at the exact ns, a multi-digit
  count, and rejections (empty, `0d`, `1`, `d`, `1 d`, `1D`, `1x`, an overflowing
  value). Command:
  `brokkr test -p mogwai-server parse_duration`
- `write_bars` empty-fill (the crux) - a crafted sequence per case:
  (a) LEADING gap (`start` a boundary, first trade several windows in) asserts NO
  leading empty rows, first row is the first fully-in-range window;
  (b) INTERIOR desert (trades, gap of several windows, trades) asserts
  carry-forward empties with `volume=0,count=0` and the carried price;
  (c) TRAILING desert to `end` with the last window's rotating trade absent
  asserts the END-FLUSH of the complete window plus trailing empties;
  (d) STRADDLE case: a late trade whose window `close_ts > end` - assert its
  window is dropped AND the interior empties before it (all closing `<= end`)
  still render (the B2 regression);
  (e) a window whose open `< start` (non-boundary `start`) is dropped, keeping the
  contract "fully inside `[start, end)`". Command:
  `brokkr test -p mogwai-server gen_bars`
- `write_trades`: assert the header and that trades are emitted in order with
  `Decimal` values printed losslessly and the right aggressor word. Command:
  `brokkr test -p mogwai-server gen_trades`
- Reproduction: assert `build_source` with no overrides for `BTCUSDT` uses
  `seed_for("BTCUSDT")` and yields the SAME first N trades as a `GeneratedSource`
  built directly from the default profile at the same `start`, so the CLI does
  not diverge the walk from the server's construction. Command:
  `brokkr test -p mogwai-server gen_reproduces`
- End-to-end: call `gen::run` with a `bars` `GenArgs` (a boundary-aligned `start`
  so the count is exact) into a buffer; assert the CSV parses and its non-empty
  bar count is consistent with the crafted/real trades over `length / interval`.
  Command:
  `brokkr test -p mogwai-server gen_run`
- Whole-landing gate (gremlins, clippy, all crates' tests):
  `brokkr check`
- Manual eyeball (not a gate): `brokkr run -p mogwai-server -- gen --type bars
  --length 1d --interval 5m` prints a day of 5-minute bars whose `trade_count`
  column maps the deserts.

## Keep / revert

Kept only if `brokkr check` is green and the `gen_bars` tests pin all five cases
(leading skip, interior fill, trailing fill + end-flush, straddle drop still
fills interior, start-straddle drop) - that logic is the tool's whole value for
D16 and the one place a subtle bug hides. Because the landing is purely additive
(a new subcommand plus two widened visibilities), it cannot regress the server or
adapter; if any pre-existing test changes, something leaked outside the new
module and the landing is reverted whole and reworked.
